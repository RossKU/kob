//! Tests of the archive pruning on hand-built stores.
//!
//! Exactly the final dead rows go. These stay:
//!
//! - the rows of a live order,
//! - the rows of an order whose closing a revert could undo,
//! - the rows of an order that a gap closed,
//! - a genesis row whose value is missing on the order,
//! - the amends of the window,
//! - the holdings of the retention.
//!
//! The proof of a token's program survives the pruning of its holdings (both prunings). A live
//! order's spent custody stays under the lean pruning too.

use super::*;
use crate::indexer::db::{meta_get, open_memory};
use crate::indexer::lean::{self, LeanConfig};

/// The window: chain blocks 100 to 110 (`MIN(blocks.seq)` = 100) and DAA 1000 to 1010.
///
/// Rows stamped below block 100 are final.
const FINAL: i64 = 100;

/// Name of the `k`-th token program this build pins (`holding_programs` names them).
fn program(k: usize) -> &'static str {
    kob_protocol::artifacts::TemplateId::TOKEN_PROGRAMS[k].name()
}

fn h(n: u8) -> Vec<u8> {
    vec![n; 32]
}

/// A hand-built store: five orders with their rows, amends and holdings.
///
/// Some orders are live and some are closed, and some of their rows are final (see the comments).
fn store() -> Connection {
    let c = open_memory("testnet-10").unwrap();
    c.execute_batch(
        "WITH RECURSIVE n(i) AS (SELECT 100 UNION ALL SELECT i + 1 FROM n WHERE i < 110) \
         INSERT INTO blocks (seq, hash, daa) SELECT i, CAST(printf('b%031d', i) AS BLOB), 900 + i FROM n;",
    )
    .unwrap();
    for n in [0xaa, 0xbb, 0xcc, 1, 2, 3, 4, 5] {
        c.execute("INSERT OR IGNORE INTO ids (h) VALUES (?1)", [h(n)]).unwrap();
    }
    let id = |n: u8| -> i64 { c.query_row("SELECT id FROM ids WHERE h = ?1", [h(n)], |r| r.get(0)).unwrap() };
    let (tok, tpl, key) = (id(0xaa), id(0xbb), id(0xcc));
    // Order `k` has covenant id `h(k)`, which is also an `ids` row: the owner of its custody
    // holdings. Its genesis output is `(h(10 + k), 0)`.
    let order = |k: u8, status: &str, last_block: i64, genesis_value: Option<i64>| {
        c.execute(
            "INSERT INTO orders (id, covenant_id, contract, template_id, family, side, token_id, token_tpl_id, in_book, genesis_state, \
             genesis_txid, genesis_out, genesis_block, genesis_daa, listed, genesis_value) \
             VALUES (?1, ?2, 'KobAsk', ?3, 1, 1, ?3, ?4, 1, zeroblob(8), ?5, 0, 1, 1, 1, ?6)",
            rusqlite::params![k as i64, h(k), tok, tpl, h(10 + k), genesis_value],
        )
        .unwrap();
        c.execute(
            "INSERT INTO order_state (order_id, status, filled_amount, state_known, last_block, last_daa) VALUES (?1, ?2, 0, 1, ?3, ?3)",
            rusqlite::params![k as i64, status, last_block],
        )
        .unwrap();
    };
    let utxo = |k: u8, tx: u8, spent: Option<i64>| {
        c.execute(
            "INSERT INTO order_utxos (txid, idx, order_id, value, spk, created_block, created_daa, spent_block) VALUES (?1, 0, ?2, 7, x'00', 1, 1, ?3)",
            rusqlite::params![h(tx), k as i64, spent],
        )
        .unwrap();
    };
    let tutxo = |k: u8, tx: u8, spent: Option<i64>| {
        c.execute(
            "INSERT INTO token_utxos (txid, idx, token_id, order_id, amount, value, role, created_block, created_daa, spent_block) \
             VALUES (?1, 1, ?2, ?3, 5, 3, 'custody', 1, 1, ?4)",
            rusqlite::params![h(tx), tok, k as i64, spent],
        )
        .unwrap();
    };
    // 1: Filled, closed by a final spend. Every spent row goes.
    order(1, "filled", 20, Some(7));
    utxo(1, 11, Some(10));
    utxo(1, 21, Some(20));
    tutxo(1, 31, Some(20));
    // 2: Live. Its spent rows stay.
    order(2, "partial", 15, Some(7));
    utxo(2, 12, Some(15));
    utxo(2, 22, None);
    tutxo(2, 32, Some(15));
    tutxo(2, 42, None);
    // 3: Closed, but the order has no genesis value, as in a store migrated without the genesis
    // row. Its genesis row stays and the other row goes.
    order(3, "cancelled", 30, None);
    utxo(3, 13, Some(12));
    utxo(3, 23, Some(30));
    // 4: Closed in a block of the window. A revert could make it live again, so it keeps everything
    // until then.
    order(4, "filled", 105, Some(7));
    utxo(4, 14, Some(50));
    utxo(4, 24, Some(105));
    // 5: Closed by a gap (spent at block 0). A recovery import can revive it at those rows.
    order(5, "closed", 40, Some(7));
    utxo(5, 15, Some(0));
    tutxo(5, 35, Some(0));
    // In-place amends: one of a final block and one of the window.
    for (k, block) in [(1, 10), (2, 105)] {
        c.execute("INSERT INTO order_amends (order_id, block_seq, listed) VALUES (?1, ?2, 1)", [k, block]).unwrap();
    }
    // Holdings: (tx, owner, role, created_daa, spent_block).
    for (tx, owner, role, daa, spent) in [
        (51u8, key, "owned", 10i64, Some(10i64)), // old, spent, final: goes
        (52, key, "owned", 995, Some(10)),        // spent and final, but created within the retention: stays
        (53, key, "owned", 10, None),             // unspent: stays
        (54, id(2), "custody", 10, Some(15)),     // a live order's spent custody: stays
        (55, id(1), "custody", 10, Some(20)),     // a closed order's spent custody: goes
        (56, key, "owned", 10, Some(105)),        // spent in the window: stays
    ] {
        c.execute(
            "INSERT INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, \
             created_daa, spent_block) VALUES (?1, 0, ?2, ?7, 1, ?3, 0, 5, 1, x'08', ?4, 1, ?5, ?6)",
            rusqlite::params![h(tx), tok, owner, role, daa, spent, program(0)],
        )
        .unwrap();
    }
    c
}

/// The first byte of the transaction id of each row that `sql` returns.
fn keys(c: &Connection, sql: &str) -> Vec<u8> {
    let mut st = c.prepare(sql).unwrap();
    st.query_map([], |r| Ok(r.get::<_, Vec<u8>>(0)?[0])).unwrap().map(|r| r.unwrap()).collect()
}

fn programs(c: &Connection) -> Vec<(i64, String, i64)> {
    let mut st = c.prepare("SELECT token_id, program, n FROM holding_programs ORDER BY 1, 2").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
}

/// Runs batches at cursor DAA 1010 with a window of 10 DAA until one finds nothing more.
///
/// Returns the number of rows deleted.
fn prune_all(c: &mut Connection, cfg: &ArchiveConfig) -> usize {
    let mut rows = 0;
    for _ in 0..1_000 {
        let r = prune(c, cfg, 1010, None, 10).unwrap();
        rows += r.rows;
        if !r.more {
            return rows;
        }
    }
    panic!("the pruning never ended");
}

#[test]
fn the_archive_pruning_deletes_exactly_the_final_dead_rows() {
    let mut c = store();
    let before = programs(&c);
    assert_eq!(before.len(), 1);
    // Spent holdings created before DAA 1010 - 15 = 995 go. With one row a step, every step
    // boundary is crossed.
    let cfg = ArchiveConfig { step_rows: 1, ..ArchiveConfig::new(15) };
    let rows = prune_all(&mut c, &cfg);
    assert_eq!(
        keys(&c, "SELECT txid FROM order_utxos ORDER BY txid"),
        vec![12, 13, 14, 15, 22, 24],
        "order 1 lost both rows, order 3 the one that is not its genesis; the live, the revertible and the gap-closed orders keep theirs"
    );
    assert_eq!(keys(&c, "SELECT txid FROM token_utxos ORDER BY txid"), vec![32, 35, 42]);
    assert_eq!(c.query_row("SELECT group_concat(block_seq) FROM order_amends", [], |r| r.get::<_, String>(0)).unwrap(), "105");
    assert_eq!(keys(&c, "SELECT txid FROM token_holdings ORDER BY txid"), vec![52, 53, 54, 56]);
    // 3 order UTXOs, 1 token UTXO, 1 amend and 2 holdings.
    assert_eq!(rows, 7);
    // The proof of the token's program stays, and its count is not taken back.
    assert_eq!(programs(&c), before);
    // The batch leaves no trace of itself and remembers where it got to.
    assert_eq!(meta_get(&c, "pruning").unwrap(), None);
    assert_eq!(meta_get(&c, MARK_KEY).unwrap().as_deref(), Some(FINAL.to_string().as_str()));
    assert_eq!(meta_get(&c, "pruned").unwrap(), None, "the store still holds every order and event");
    // Nothing more goes, however often it runs.
    assert_eq!(prune_all(&mut c, &cfg), 0);

    // The window moves past block 105. The closing of order 4 is final, so its rows go, and so does
    // the amend.
    c.execute("DELETE FROM blocks WHERE seq < 108", []).unwrap();
    prune_all(&mut c, &cfg);
    assert_eq!(keys(&c, "SELECT txid FROM order_utxos ORDER BY txid"), vec![12, 13, 15, 22]);
    assert_eq!(c.query_row("SELECT COUNT(*) FROM order_amends", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    // Order 2 closes in block 109. Once that is final, its earlier spent rows (visited while it was
    // live) go too.
    c.execute("UPDATE order_utxos SET spent_block = 109 WHERE txid = ?1", [h(22)]).unwrap();
    c.execute("UPDATE token_utxos SET spent_block = 109 WHERE txid = ?1", [h(42)]).unwrap();
    c.execute("UPDATE order_state SET status = 'filled', last_block = 109 WHERE order_id = 2", []).unwrap();
    prune_all(&mut c, &cfg);
    assert_eq!(keys(&c, "SELECT txid FROM order_utxos ORDER BY txid"), vec![12, 13, 15, 22], "block 109 is not final yet");
    c.execute("DELETE FROM blocks WHERE seq < 110", []).unwrap();
    prune_all(&mut c, &cfg);
    assert_eq!(keys(&c, "SELECT txid FROM order_utxos ORDER BY txid"), vec![13, 15]);
    assert_eq!(keys(&c, "SELECT txid FROM token_utxos ORDER BY txid"), vec![35]);
    // Its spent custody holding is no longer a live order's.
    assert_eq!(keys(&c, "SELECT txid FROM token_holdings ORDER BY txid"), vec![52, 53]);
    assert_eq!(programs(&c), before);
}

#[test]
fn a_retention_that_keeps_everything_deletes_no_holding_and_a_batch_is_bounded() {
    let mut c = store();
    let cfg = ArchiveConfig { rows_per_batch: 1, ..ArchiveConfig::new(u64::MAX / 8) };
    // One batch: the limit ends it after its first deletion, with more to do.
    let r = prune(&mut c, &cfg, 1010, None, 10).unwrap();
    assert!(r.more && r.rows >= 1, "{r:?}");
    prune_all(&mut c, &cfg);
    assert_eq!(keys(&c, "SELECT txid FROM token_holdings ORDER BY txid"), vec![51, 52, 53, 54, 55, 56]);
    assert_eq!(keys(&c, "SELECT txid FROM order_utxos ORDER BY txid"), vec![12, 13, 14, 15, 22, 24]);
}

/// The lean pruning keeps a live order's spent custody, and no pruning takes a program proof back.
///
/// A pair order's next custody is checked against the spent custody. The token list, the listing
/// rules and the standard scale read the program proof of a token.
#[test]
fn the_lean_pruning_keeps_a_live_orders_custody_and_no_pruning_takes_a_program_back() {
    let mut c = store();
    let before = programs(&c);
    lean::ensure_indexes(&c).unwrap();
    let cfg = LeanConfig { budget: Duration::from_secs(60), ..LeanConfig::new(15, 0) };
    for _ in 0..100 {
        if !lean::prune(&mut c, &cfg, 1010, None, 10).unwrap().more {
            break;
        }
    }
    let held = keys(&c, "SELECT txid FROM token_holdings ORDER BY txid");
    assert!(held.contains(&54), "the live order's spent custody stays: {held:?}");
    assert!(!held.contains(&51) && !held.contains(&55), "{held:?}");
    assert_eq!(programs(&c), before);
    // Every holding is pruned. The program is still proved.
    c.execute_batch(
        "INSERT INTO meta (k, v) VALUES ('pruning', '1'); DELETE FROM token_holdings; DELETE FROM meta WHERE k = 'pruning';",
    )
    .unwrap();
    assert_eq!(programs(&c), before);
    assert_eq!(crate::indexer::reads::proven_programs(&c, &h(0xaa)).unwrap().len(), 1);
    // A delete that is not the pruning, such as a revert, takes back what its insert counted.
    c.execute(
        "INSERT INTO token_holdings (txid, idx, token_id, program, family, owner_id, owner_kind, amount, value, state, role, created_block, created_daa) \
         VALUES (?1, 0, (SELECT id FROM ids WHERE h = ?2), ?3, 2, 1, 0, 5, 1, x'08', 'owned', 1, 1)",
        rusqlite::params![h(60), h(0xaa), program(1)],
    )
    .unwrap();
    assert_eq!(programs(&c).len(), 2);
    c.execute("DELETE FROM token_holdings WHERE txid = ?1", [h(60)]).unwrap();
    assert_eq!(programs(&c), before);
}
