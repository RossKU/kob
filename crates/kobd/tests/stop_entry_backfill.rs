//! Regression: the backfill of the stop entries' books, which runs at every open, commits once.
//!
//! It refreshed every live if-done entry in a book with one autocommit (one sync) each, and summed
//! each entry's fills. With 10^5 entries a restart took minutes.

use kobd::indexer::db::{backfill_stop_entry_books, open_memory};
use std::sync::atomic::{AtomicUsize, Ordering};

static COMMITS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_commit(_: *mut std::os::raw::c_void) -> std::os::raw::c_int {
    COMMITS.fetch_add(1, Ordering::SeqCst);
    0
}

#[test]
fn the_stop_entry_backfill_is_one_transaction_and_keeps_the_filled_amounts() {
    const N: i64 = 300;
    let c = open_memory("testnet-10").unwrap();
    c.execute_batch("INSERT INTO ids (id, h) VALUES (1, zeroblob(32)), (2, randomblob(32));").unwrap();
    // N live if-done sell entries in their book, each with a filled amount and no fill events: a
    // recount would set it to 0.
    c.execute(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
         INSERT INTO orders (id, covenant_id, contract, template_id, family, side, token_id, in_book, genesis_state, genesis_block, \
         genesis_daa, listed) SELECT i, randomblob(32), 'KobIfdAsk', 1, 1, 1, 2, 1, zeroblob(8), 1, 1, 1 FROM n",
        [N],
    )
    .unwrap();
    c.execute(
        "INSERT INTO order_state (order_id, status, filled_amount, state_known, last_block, last_daa) SELECT id, 'partial', 7, 0, 1, 1 FROM orders",
        [],
    )
    .unwrap();
    COMMITS.store(0, Ordering::SeqCst);
    // SAFETY: the handle is the open connection's own, the hook is a plain function, and it is
    // removed before the connection is dropped.
    unsafe { rusqlite::ffi::sqlite3_commit_hook(c.handle(), Some(count_commit), std::ptr::null_mut()) };
    backfill_stop_entry_books(&c).unwrap();
    unsafe { rusqlite::ffi::sqlite3_commit_hook(c.handle(), None, std::ptr::null_mut()) };
    assert_eq!(COMMITS.load(Ordering::SeqCst), 1, "one commit for {N} entries");
    let kept: i64 = c.query_row("SELECT COUNT(*) FROM order_state WHERE filled_amount = 7", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, N, "the refresh keeps the stored filled amounts");
}
