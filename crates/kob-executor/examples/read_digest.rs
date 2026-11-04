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

fn has_col(conn: &Connection, table: &str, col: &str) -> bool {
    conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2", [table, col], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0
}

/// The columns of `table` in order, the packed ones decoded when this build has the functions (`orders.genesis_state`, `order_events.detail`).
fn columns_with(conn: &Connection, table: &str) -> String {
    let mut st = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')")).expect("table info");
    let names: Vec<String> = st.query_map([], |r| r.get(0)).expect("columns").map(|r| r.expect("column")).collect();
    names
        .iter()
        .map(|n| match (table, n.as_str()) {
            ("orders", "genesis_state") if has_fn(conn, "kob_gstate") => {
                "kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id)".to_string()
            }
            ("order_events", "detail") if has_fn(conn, "kob_dtext") => "kob_dtext(detail)".to_string(),
            _ => n.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
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
        // schema 9 names the token and maker of an order by id: the same samples through the join
        let v9 = has_col(&conn, "orders", "maker_id");
        let tokens_top = blobs(
            &conn,
            if v9 {
                "SELECT t.h FROM (SELECT token_id, COUNT(*) c FROM orders WHERE token_id IS NOT NULL GROUP BY 1) x JOIN ids t ON t.id = x.token_id ORDER BY x.c DESC, t.h LIMIT 12"
            } else {
                "SELECT token_cov_id FROM (SELECT token_cov_id, COUNT(*) c FROM orders WHERE token_cov_id IS NOT NULL GROUP BY 1 ORDER BY c DESC, token_cov_id LIMIT 12)"
            },
        );
        let makers_top = blobs(
            &conn,
            if v9 {
                "SELECT m.h FROM (SELECT maker_id, COUNT(*) c FROM orders WHERE maker_id IS NOT NULL GROUP BY 1) x JOIN ids m ON m.id = x.maker_id ORDER BY x.c DESC, m.h LIMIT 6"
            } else {
                "SELECT maker FROM (SELECT maker, COUNT(*) c FROM orders WHERE maker IS NOT NULL GROUP BY 1 ORDER BY c DESC, maker LIMIT 6)"
            },
        );
        // schema 8 keeps the owners and the order of a state row as integers: the same samples through the join
        let interned = has_col(&conn, "orders", "id");
        let owners_top = blobs(
            &conn,
            if interned {
                "SELECT w.h FROM (SELECT owner_id, COUNT(*) c FROM token_holdings GROUP BY 1) x JOIN ids w ON w.id = x.owner_id ORDER BY x.c DESC, w.h LIMIT 6"
            } else {
                "SELECT owner FROM (SELECT owner, COUNT(*) c FROM token_holdings GROUP BY 1 ORDER BY c DESC, owner LIMIT 6)"
            },
        );
        let owners_mid = blobs(
            &conn,
            if interned {
                "SELECT w.h FROM token_holdings h JOIN ids w ON w.id = h.owner_id WHERE h.rowid IN (777, 1500000, 3000000) ORDER BY h.rowid"
            } else {
                "SELECT owner FROM token_holdings WHERE rowid IN (777, 1500000, 3000000)"
            },
        );
        let live = blobs(
            &conn,
            if interned {
                "SELECT o.covenant_id FROM order_state s JOIN orders o ON o.id = s.order_id WHERE s.status IN ('open','partial') ORDER BY o.covenant_id LIMIT 120"
            } else {
                "SELECT covenant_id FROM order_state WHERE status IN ('open','partial') ORDER BY covenant_id LIMIT 120"
            },
        );
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
        let interned = has_col(&conn, "orders", "id");
        let ustate = if has_fn(&conn, "kob_ustate") {
            "kob_ustate(u.state, (SELECT kob_gstate(o.genesis_state, o.maker, o.token_cov_id, o.token_tpl_hash, o.quote_cov_id) FROM orders o WHERE o.covenant_id = u.covenant_id))"
        } else {
            "u.state"
        };
        let hstate =
            if has_fn(&conn, "kob_hstate") { "kob_hstate(h.state, h.family, h.owner, h.owner_kind, h.amount)" } else { "h.state" };
        if interned {
            // schema 8: the same rows, the same columns in the same order, with the ids joined back to their 32 bytes
            let v9 = has_col(&conn, "orders", "maker_id");
            let (gen, joins) = if v9 {
                (
                    "kob_gstate(o.genesis_state, o_mk.h, o_tk.h, o_tp.h, o_qt.h)".to_string(),
                    " LEFT JOIN ids o_mk ON o_mk.id = o.maker_id LEFT JOIN ids o_tk ON o_tk.id = o.token_id LEFT JOIN ids o_tp ON o_tp.id = o.token_tpl_id LEFT JOIN ids o_qt ON o_qt.id = o.quote_id LEFT JOIN ids o_tm ON o_tm.id = o.template_id LEFT JOIN ids o_ex ON o_ex.id = o.ext_id ".to_string(),
                )
            } else {
                ("kob_gstate(o.genesis_state, o.maker, o.token_cov_id, o.token_tpl_hash, o.quote_cov_id)".to_string(), " ".to_string())
            };
            table_digest(
                &conn,
                "order_utxos",
                &format!("SELECT u.txid, u.idx, o.covenant_id, u.value, u.spk, kob_ustate(u.state, {gen}), u.created_block, u.created_daa, u.spent_block, u.spent_txid, u.spent_entry, u.spent_amount FROM order_utxos u JOIN orders o ON o.id = u.order_id{joins}ORDER BY u.rowid"),
            );
            table_digest(
                &conn,
                "token_holdings",
                "SELECT h.txid, h.idx, t.h, h.program, h.family, w.h, h.owner_kind, h.amount, h.value, kob_hstate(h.state, h.family, w.h, h.owner_kind, h.amount), h.role, h.created_block, h.created_daa, h.spent_block, h.spent_txid FROM token_holdings h JOIN ids t ON t.id = h.token_id JOIN ids w ON w.id = h.owner_id ORDER BY h.rowid",
            );
            table_digest(
                &conn,
                "order_events",
                "SELECT e.id, o.covenant_id, e.block_seq, e.daa, e.ts, e.txid, e.tx_pos, e.kind, t.h, e.side, e.amount, e.price, e.payout, e.closes, kob_dtext(e.detail) FROM order_events e JOIN orders o ON o.id = e.order_id LEFT JOIN ids t ON t.id = e.token_id ORDER BY e.id",
            );
            table_digest(
                &conn,
                "order_amends",
                "SELECT a.id, o.covenant_id, a.block_seq, a.price, a.tip, a.tif, a.expiry_daa, a.active_from, a.deadline, a.listed, a.unlisted_reason, a.budget_rate, a.reserve, a.min_fill FROM order_amends a JOIN orders o ON o.id = a.order_id ORDER BY a.id",
            );
            table_digest(
                &conn,
                "order_state",
                "SELECT o.covenant_id, s.status, s.filled_amount, s.remaining_amount, s.amount_exact, s.cur_txid, s.cur_idx, s.cur_value, s.state_known, s.last_block, s.last_daa FROM order_state s JOIN orders o ON o.id = s.order_id ORDER BY o.covenant_id",
            );
            table_digest(
                &conn,
                "token_utxos",
                "SELECT u.txid, u.idx, t.h, o.covenant_id, u.amount, u.value, u.role, u.created_block, u.created_daa, u.spent_block, u.spent_txid FROM token_utxos u JOIN ids t ON t.id = u.token_id JOIN orders o ON o.id = u.order_id ORDER BY u.txid, u.idx",
            );
            let orders_sql = if v9 {
                format!("SELECT o.covenant_id, o.contract, o_tm.h, o.family, o.side, o_mk.h, o_tk.h, o_tp.h, o_ex.h, o.scale, o.min_fill, o.price, o.tip, o.tif, o.expiry_daa, o.active_from, o.in_book, o.budget_rate, o.reserve, o.initial_amount, o.deadline, {gen}, o.genesis_txid, o.genesis_out, o.genesis_block, o.genesis_daa, o.parent, o.listed, o.unlisted_reason, o.origin, o_qt.h FROM orders o{joins}ORDER BY o.rowid")
            } else {
                "SELECT covenant_id, contract, template_hash, family, side, maker, token_cov_id, token_tpl_hash, ext_commit, scale, min_fill, price, tip, tif, expiry_daa, active_from, in_book, budget_rate, reserve, initial_amount, deadline, kob_gstate(genesis_state, maker, token_cov_id, token_tpl_hash, quote_cov_id), genesis_txid, genesis_out, genesis_block, genesis_daa, parent, listed, unlisted_reason, origin, quote_cov_id FROM orders ORDER BY rowid".to_string()
            };
            table_digest(&conn, "orders", &orders_sql);
        } else {
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
        }
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
            // the tables that name orders, tokens or owners by integer were digested above, joined back
            let done = interned && matches!(t, "orders" | "order_events" | "order_amends" | "order_state" | "token_utxos");
            if exists > 0 && !done {
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
                // the packed columns decoded when this build has the functions that decode them
                let list = columns_with(&conn, t);
                table_digest(&conn, t, &format!("SELECT {list} FROM {t} ORDER BY {order}"));
            }
        }
        table_digest(
            &conn,
            "meta",
            "SELECT k, v FROM meta WHERE k NOT IN ('schema_version', 'holding_programs') AND k NOT LIKE 'storage%' ORDER BY k",
        );
    }
}
