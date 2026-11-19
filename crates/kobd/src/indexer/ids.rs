//! Integer surrogate keys that stand in for 32-byte hashes in the big tables.
//!
//! The big tables would otherwise repeat the same hashes in every row and index entry: the
//! covenant id of an order in each of its events, UTXOs and amends, the covenant id of a token in
//! each of its fills and holdings, and the owner of a holding in each of its index entries. Each
//! hash is stored once and referred to by an integer:
//!
//! - An order is `orders.id`, an `INTEGER PRIMARY KEY AUTOINCREMENT`. It is never reused, so the
//!   id of a reverted order never names a later one. `order_events.order_id`,
//!   `order_utxos.order_id`, `order_state.order_id`, `order_amends.order_id` and
//!   `token_utxos.order_id` (the order that owns the token UTXO) refer to it. The covenant id
//!   stays in `orders`: the order lists are ordered by it, and it names an order outside the
//!   store.
//! - A token's covenant id and a holding's owner are rows of `ids` (`id`, `h`). A row is added
//!   when the value is first seen and is never deleted. `order_events.token_id`,
//!   `token_utxos.token_id`, `token_holdings.token_id` / `owner_id` and
//!   `holding_programs.token_id` refer to it.
//! - The columns of `orders` itself do the same: `maker_id`, `template_id`, `token_id`,
//!   `token_tpl_id`, `ext_id` and `quote_id`. The few values the orders name are the first rows
//!   of `ids` (`kobd db migrate` assigns them first). A new order interns its values before it
//!   is inserted, see [`intern`].
//!
//! - (Schema 14) The transaction ids of the token holdings are rows of `txs` (`id`, `h`, `n`):
//!   `token_holdings.tx_id` and `spent_tx_id` refer to it, and `n` counts them. A row is added when a
//!   holding first names the id ([`intern_tx`]) and goes with the last holding that names it
//!   (`schema.sql`). The lookup by the 32 bytes reads an index of their first 8, `txs_h`, so it must be
//!   written `tx_of!("?1")`: a plain `WHERE h = ?1` reads the whole table. The names of the token
//!   programs are rows of `programs` (`token_holdings.program_id`), and a holding's role is a small
//!   code ([`role_code`]).
//!
//! Rust code keeps using [`Hash32`]. A statement that filters or inserts by a hash resolves it
//! inside the statement, with `order_of!("?1")` or `id_of!("?2")`. Each is an uncorrelated scalar
//! subquery that does one index seek and is evaluated once. A statement that returns a hash joins
//! back to `ids`. A writer that must create the `ids` row first uses [`intern`].

use super::db::DbResult;
use rusqlite::{Connection, OptionalExtension};

/// `ids.id` of `h`, adding it when it is new.
pub fn intern(conn: &Connection, h: &[u8]) -> DbResult<i64> {
    if let Some(id) = lookup(conn, h)? {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO ids (h) VALUES (?1)")?.execute([h])?;
    Ok(conn.last_insert_rowid())
}

/// `ids.id` of `h`, if it has one.
pub fn lookup(conn: &Connection, h: &[u8]) -> DbResult<Option<i64>> {
    Ok(conn.prepare_cached("SELECT id FROM ids WHERE h = ?1")?.query_row([h], |r| r.get(0)).optional()?)
}

/// The 32 bytes of `ids.id` `id`.
pub fn hash_of(conn: &Connection, id: i64) -> DbResult<Vec<u8>> {
    Ok(conn.prepare_cached("SELECT h FROM ids WHERE id = ?1")?.query_row([id], |r| r.get(0))?)
}

/// `txs.id` of the transaction id `h`, adding it when it is new.
///
/// A new row counts no holding yet: the holding that the caller inserts, or marks spent by it, is
/// counted by the triggers of `token_holdings`. A caller interns only an id that a holding is about
/// to name, so no row is left that names nothing.
pub fn intern_tx(conn: &Connection, h: &[u8]) -> DbResult<i64> {
    if let Some(id) = lookup_tx(conn, h)? {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO txs (h, n) VALUES (?1, 0)")?.execute([h])?;
    Ok(conn.last_insert_rowid())
}

/// `txs.id` of the transaction id `h`, if a holding names it.
pub fn lookup_tx(conn: &Connection, h: &[u8]) -> DbResult<Option<i64>> {
    // `tx_of!`, which is defined below.
    Ok(conn
        .prepare_cached("SELECT id FROM txs WHERE substr(h, 1, 8) = substr(?1, 1, 8) AND h = ?1")?
        .query_row([h], |r| r.get(0))
        .optional()?)
}

/// `programs.id` of the token program `name`, adding it when it is new.
pub fn intern_program(conn: &Connection, name: &str) -> DbResult<i64> {
    if let Some(id) = conn.prepare_cached("SELECT id FROM programs WHERE name = ?1")?.query_row([name], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    conn.prepare_cached("INSERT INTO programs (name) VALUES (?1)")?.execute([name])?;
    Ok(conn.last_insert_rowid())
}

/// The roles of a token holding, by their `token_holdings.role_code`.
pub const ROLES: [&str; 4] = ["owned", "custody", "stray", super::delivery::ROLE_BOUGHT];

/// `token_holdings.role_code` of a holding role (`owned`, `custody`, `stray`, `bought`).
///
/// # Panics
///
/// On a role that is not one of [`ROLES`], which is a bug of the caller.
pub fn role_code(role: &str) -> i64 {
    ROLES.iter().position(|r| *r == role).unwrap_or_else(|| panic!("unknown holding role {role}")) as i64
}

/// `orders.id` of the order with this covenant id.
pub fn order_pk(conn: &Connection, covenant_id: &[u8]) -> DbResult<Option<i64>> {
    Ok(conn.prepare_cached("SELECT id FROM orders WHERE covenant_id = ?1")?.query_row([covenant_id], |r| r.get(0)).optional()?)
}

/// SQL for the `orders.id` of the covenant id bound at `$p`, a literal such as `"?1"`.
///
/// It is NULL when there is no such order.
macro_rules! order_of {
    ($p:literal) => {
        concat!("(SELECT id FROM orders WHERE covenant_id = ", $p, ")")
    };
}

/// SQL for the `ids.id` of the 32-byte value bound at `$p`.
///
/// It is NULL when the value has no row, which means nothing refers to it.
macro_rules! id_of {
    ($p:literal) => {
        concat!("(SELECT id FROM ids WHERE h = ", $p, ")")
    };
}
/// SQL for the `txs.id` of the 32-byte transaction id bound at (or named by) `$p`, through `txs_h`.
///
/// It is NULL when no holding names the id. `$p` appears twice, so it is a numbered parameter
/// (`?1`) or a column, never a bare `?`.
macro_rules! tx_of {
    ($p:literal) => {
        concat!("(SELECT tq.id FROM txs tq WHERE substr(tq.h, 1, 8) = substr(", $p, ", 1, 8) AND tq.h = ", $p, ")")
    };
}

/// SQL for the 32 bytes of the `txs.id` in `$c` (NULL for NULL).
macro_rules! tx_hash {
    ($c:literal) => {
        concat!("(SELECT tq.h FROM txs tq WHERE tq.id = ", $c, ")")
    };
}

/// SQL for the name of the `programs.id` in `$c`.
macro_rules! program_name {
    ($c:literal) => {
        concat!("(SELECT pq.name FROM programs pq WHERE pq.id = ", $c, ")")
    };
}

/// SQL for the role name of the `token_holdings.role_code` in `$c` ([`ROLES`]).
macro_rules! role_name {
    ($c:literal) => {
        concat!("CASE ", $c, " WHEN 0 THEN 'owned' WHEN 1 THEN 'custody' WHEN 2 THEN 'stray' WHEN 3 THEN 'bought' END")
    };
}
pub(crate) use {id_of, order_of, program_name, role_name, tx_hash, tx_of};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::db::open_memory;

    #[test]
    fn ids_are_added_once_and_looked_up() {
        let c = open_memory("testnet-10").unwrap();
        assert_eq!(lookup(&c, &[1; 32]).unwrap(), None);
        let a = intern(&c, &[1; 32]).unwrap();
        let b = intern(&c, &[2; 32]).unwrap();
        assert_ne!(a, b);
        assert_eq!(intern(&c, &[1; 32]).unwrap(), a);
        assert_eq!(lookup(&c, &[2; 32]).unwrap(), Some(b));
        let via_sql: Option<i64> = c.query_row(concat!("SELECT ", id_of!("?1")), [&[2u8; 32][..]], |r| r.get(0)).unwrap();
        assert_eq!(via_sql, Some(b));
        let none: Option<i64> = c.query_row(concat!("SELECT ", order_of!("?1")), [&[9u8; 32][..]], |r| r.get(0)).unwrap();
        assert_eq!(none, None);
    }
}
