//! Integer surrogate keys (schemas 8 and 9).
//!
//! The big tables of the store repeated the same 32-byte hashes in every row and again in every index entry: the covenant id of
//! an order in each of its events, UTXOs and amends, the covenant id of a token in each of its fills and holdings, the owner of
//! a holding in each of its index entries. They are stored once and referred to by integer instead:
//!
//! * an order is `orders.id` (an `INTEGER PRIMARY KEY AUTOINCREMENT`: never reused, so the id of a reverted order can never name
//!   a later one); `order_events.order_id`, `order_utxos.order_id`, `order_state.order_id`, `order_amends.order_id` and
//!   `token_utxos.order_id` (the order that owns the token UTXO) refer to it. The covenant id stays in `orders`: the order
//!   lists are ordered by it, and it is what an order is called outside the store;
//! * a token's covenant id and a holding's owner are rows of `ids` (`id`, `h`), added when first seen and never deleted:
//!   `order_events.token_id`, `token_utxos.token_id`, `token_holdings.token_id` / `owner_id`, `holding_programs.token_id`;
//! * (schema 9) the same for the columns of `orders` itself: `maker_id`, `template_id`, `token_id`, `token_tpl_id`, `ext_id`, `quote_id`.
//!   The few values the orders name are the first rows of `ids` (`index migrate` assigns them first), and a new order interns its
//!   values before it is inserted ([`intern`]).
//!
//! Rust keeps speaking [`Hash32`]. A statement that filters or inserts by one of them resolves it in the statement
//! (`order_of!("?1")`, `id_of!("?2")`: an uncorrelated scalar subquery, one index seek, evaluated once); a statement that returns
//! one joins back. A writer that must create the `ids` row first uses [`intern`].

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

/// `orders.id` of the order with this covenant id.
pub fn order_pk(conn: &Connection, covenant_id: &[u8]) -> DbResult<Option<i64>> {
    Ok(conn.prepare_cached("SELECT id FROM orders WHERE covenant_id = ?1")?.query_row([covenant_id], |r| r.get(0)).optional()?)
}

/// SQL: the `orders.id` of the covenant id bound at `$p` (a literal such as `"?1"`); NULL when there is no such order.
macro_rules! order_of {
    ($p:literal) => {
        concat!("(SELECT id FROM orders WHERE covenant_id = ", $p, ")")
    };
}

/// SQL: the `ids.id` of the 32-byte value bound at `$p`; NULL when it has no row (nothing refers to it).
macro_rules! id_of {
    ($p:literal) => {
        concat!("(SELECT id FROM ids WHERE h = ", $p, ")")
    };
}
pub(crate) use {id_of, order_of};

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
