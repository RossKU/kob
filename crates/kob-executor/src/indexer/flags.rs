//! "Possibly frozen" flags: orders whose engine pre-simulation failed at a token program.
//!
//! KOB has no freeze / seize mechanism of its own (`registry/README.md`): a token program that freezes a balance or blacklists an
//! address simply rejects the transaction, atomically. What KOB does is not to list such an order as live liquidity. The matcher
//! pre-executes every transaction it builds in the rusty-kaspa engine; when the token program rejects one, the order it can
//! attribute the rejection to is flagged here. The book, the depth and the token counts leave it out (`NOT_FROZEN` in
//! `reads`), the order view carries `possibly_frozen` and the reason, and the matcher re-probes it now and then: a probe that
//! passes clears the flag. A flag belongs to one order UTXO, so it lapses when the order moves.

use super::db::DbResult;
use rusqlite::{params, Connection, OptionalExtension};

/// Flags the order at its current UTXO. Returns false when the order has no known current UTXO (nothing to flag).
pub fn set(conn: &Connection, order: &[u8; 32], reason: &str, daa: u64) -> DbResult<bool> {
    let cur: Option<(Vec<u8>, i64)> = conn
        .query_row("SELECT cur_txid, cur_idx FROM order_state WHERE covenant_id = ?1 AND cur_txid IS NOT NULL", [&order[..]], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((txid, idx)) = cur else { return Ok(false) };
    conn.execute(
        "INSERT INTO order_flags (covenant_id, txid, idx, reason, set_daa) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(covenant_id) DO UPDATE SET txid = excluded.txid, idx = excluded.idx, reason = excluded.reason, \
         set_daa = excluded.set_daa",
        params![&order[..], txid, idx, reason, daa as i64],
    )?;
    Ok(true)
}

/// Removes the flag (a later pre-simulation passed). Returns whether a flag was there.
pub fn clear(conn: &Connection, order: &[u8; 32]) -> DbResult<bool> {
    Ok(conn.execute("DELETE FROM order_flags WHERE covenant_id = ?1", [&order[..]])? > 0)
}

/// The reason of the flag on the order's current UTXO, if any.
pub fn reason(conn: &Connection, order: &[u8; 32]) -> DbResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT f.reason FROM order_flags f JOIN order_state s ON s.covenant_id = f.covenant_id \
             WHERE f.covenant_id = ?1 AND f.txid = s.cur_txid AND f.idx = s.cur_idx",
            [&order[..]],
            |r| r.get(0),
        )
        .optional()?)
}
