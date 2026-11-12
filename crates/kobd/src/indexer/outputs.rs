//! What the inputs of a recorded transaction held, from the outputs the store itself recorded: the network fee of an executor's
//! transaction whose record has no input values (`record::TxRecord::in_value`, written since 2026-11-12: every older frame, and so a
//! `kobd db replay` of the history).
//!
//! An input is resolved by the output it spends: a tracked order UTXO (`order_utxos`), an order's custody or stray (`token_utxos`), a
//! proven token holding (`token_holdings`), or a plain output of an earlier recorded transaction (`recorded_outputs`): an Executor's
//! change, which funds its next transaction, a maker's payout or refund that funds a placement. A transaction's input total is known
//! only when every one of its inputs resolves (its first funding, from outside KOB, does not).
//!
//! `recorded_outputs` stays small: a row is the plain output of a recorded transaction, stamped with its block (a revert deletes it
//! and un-spends what its block spent); a spent row goes once its spend is final (out of the reorg window), and an unspent one once it is
//! older than [`RETENTION_DAA`] (a payout its maker keeps, a delivery's carrier: most plain outputs of KOB transactions are never spent
//! by one). A lookup ignores a row older than the retention at the spending block's DAA score, so when the pruning ran makes no
//! difference: the live path and a replay resolve the same inputs.

use super::db::DbResult;
use super::record::TxRecord;
use crate::hex::Hash32;
use rusqlite::{params, Connection, OptionalExtension};

/// How long an unspent recorded output is kept and resolves an input: 7 days at 10 DAA per second. An Executor spends its change within
/// minutes; a funding UTXO left longer is resolved by the node's own input values from 2026-11-12 on.
pub const RETENTION_DAA: u64 = 7 * 86_400 * 10;

/// The value of the output `txid:idx` that an input of a transaction of the block at `daa` spends, when the store knows it: an order's
/// custody or stray, a token holding (unspent rows), or a recorded plain output within the retention.
pub fn input_value(conn: &Connection, txid: &Hash32, idx: u32, daa: u64) -> DbResult<Option<i64>> {
    for sql in [
        "SELECT value FROM token_utxos WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL",
        "SELECT value FROM token_holdings WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL",
    ] {
        if let Some(v) = conn.prepare_cached(sql)?.query_row(params![&txid.0[..], idx], |r| r.get(0)).optional()? {
            return Ok(Some(v));
        }
    }
    Ok(conn
        .prepare_cached(
            "SELECT value FROM recorded_outputs WHERE txid = ?1 AND idx = ?2 AND spent_block IS NULL AND created_daa >= ?3",
        )?
        .query_row(params![&txid.0[..], idx, daa.saturating_sub(RETENTION_DAA) as i64], |r| r.get(0))
        .optional()?)
}

/// The record's inputs spend recorded outputs, and its plain outputs are recorded (block `seq`, DAA score `daa`).
pub fn record(conn: &Connection, rec: &TxRecord, seq: i64, daa: u64) -> DbResult<()> {
    for i in &rec.inputs {
        conn.prepare_cached(
            "UPDATE recorded_outputs SET spent_block = ?1, spent_daa = ?2 WHERE txid = ?3 AND idx = ?4 AND spent_block IS NULL",
        )?
        .execute(params![seq, daa as i64, &i.txid.0[..], i.index])?;
    }
    for (j, o) in rec.outputs.iter().enumerate().filter(|(_, o)| o.cov.is_none()) {
        conn.prepare_cached(
            "INSERT OR IGNORE INTO recorded_outputs (txid, idx, value, created_block, created_daa) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?
        .execute(params![&rec.txid.0[..], j as u32, o.value as i64, seq, daa as i64])?;
    }
    Ok(())
}

/// Undoes block `seq` (a revert): its outputs go, what it spent is unspent.
pub fn revert(conn: &Connection, seq: i64) -> DbResult<usize> {
    let n = conn.execute("DELETE FROM recorded_outputs WHERE created_block = ?1", [seq])?;
    Ok(n + conn.execute("UPDATE recorded_outputs SET spent_block = NULL, spent_daa = NULL WHERE spent_block = ?1", [seq])?)
}

/// Drops what no later input can need at cursor DAA `daa`: the rows spent before the reorg window (`window` DAA) and the unspent ones past
/// the retention.
pub fn prune(conn: &Connection, daa: u64, window: u64) -> DbResult<usize> {
    let n = conn
        .execute("DELETE FROM recorded_outputs WHERE spent_daa IS NOT NULL AND spent_daa < ?1", [daa.saturating_sub(window) as i64])?;
    Ok(n + conn.execute(
        "DELETE FROM recorded_outputs WHERE spent_daa IS NULL AND created_daa < ?1",
        [daa.saturating_sub(RETENTION_DAA.max(window)) as i64],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::record::{RecIn, RecOut};

    fn rec(txid: u8, spends: &[(u8, u32)], outs: &[u64]) -> TxRecord {
        TxRecord {
            txid: Hash32([txid; 32]),
            pos: 0,
            payload: vec![],
            inputs: spends.iter().map(|(t, i)| RecIn { txid: Hash32([*t; 32]), index: *i, cov: None, daa: 0, reveal: None }).collect(),
            outputs: outs.iter().map(|v| RecOut { value: *v, spk: vec![], cov: None }).collect(),
            tok_outs: vec![],
            holds: vec![],
            kcc23: None,
            payer: None,
            in_value: None,
        }
    }

    /// A recorded output resolves an input until it is spent or older than the retention (whenever the pruning ran); a revert
    /// un-spends; the pruning drops what is spent and final, and what is unspent past the retention.
    #[test]
    fn recorded_outputs_resolve_until_spent_or_old() {
        let c = crate::indexer::db::open_memory("testnet-10").unwrap();
        record(&c, &rec(1, &[], &[700, 300]), 1, 1_000).unwrap();
        let v = |idx, daa| input_value(&c, &Hash32([1; 32]), idx, daa).unwrap();
        assert_eq!((v(0, 2_000), v(1, 2_000), v(2, 2_000)), (Some(700), Some(300), None));
        assert_eq!(v(0, 1_000 + RETENTION_DAA), Some(700));
        assert_eq!(v(0, 1_001 + RETENTION_DAA), None, "past the retention, pruned or not");
        // block 2 spends output 0
        record(&c, &rec(2, &[(1, 0)], &[650]), 2, 2_000).unwrap();
        assert_eq!(v(0, 3_000), None);
        assert_eq!(revert(&c, 2).unwrap(), 2, "its output goes, the spend is undone");
        assert_eq!(v(0, 3_000), Some(700));
        record(&c, &rec(2, &[(1, 0)], &[650]), 3, 2_000).unwrap();
        // the spend of block 3 is in the window at 2 100 (window 500): kept; final at 2 600: pruned
        assert_eq!(prune(&c, 2_100, 500).unwrap(), 0);
        assert_eq!(prune(&c, 2_600, 500).unwrap(), 1);
        // the unspent ones go past the retention
        assert_eq!(prune(&c, 2_000 + RETENTION_DAA, 500).unwrap(), 1, "output 1 of the first");
        let left: i64 = c.query_row("SELECT COUNT(*) FROM recorded_outputs", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1, "the second's change, created at 2 000");
    }
}
