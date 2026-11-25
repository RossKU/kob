//! The first open of a store an earlier build wrote does one-time work before anything else runs (`indexer::db`
//! `migrate`): counters, activity totals, the creator locks, the live books, the pair volume. A stop, a kill or a host
//! shutdown can come in the middle of it. Each step is one transaction with its own "done" marker in `meta`, committed
//! with its rows, so a step either completed or is done again at the next open. (The steps that keep no marker, the
//! priced flag of fills and the KAS locked and actor counters, redo whatever is missing at every open.)

mod common;

use common::*;
use kob_protocol::state::*;
use kobd::indexer::db::open_writer;
use kobd::testkit::*;

/// The derived tables the one-time steps write, as text.
fn derived(conn: &rusqlite::Connection) -> String {
    let mut out = String::new();
    for (t, order) in [
        ("counters", "name"),
        ("activity", "iv, t"),
        ("actors", "1"),
        ("kcc23_auto_live", "1"),
        ("book_live", "1"),
        ("pair_live", "1"),
        ("pair_volume", "1"),
    ] {
        let mut st = conn.prepare(&format!("SELECT * FROM {t} ORDER BY {order}")).unwrap();
        let n = st.column_count();
        let rows: Vec<String> = st
            .query_map([], |r| {
                let mut s = String::new();
                for i in 0..n {
                    s.push_str(&format!("{:?}|", r.get_ref(i)?));
                }
                Ok(s)
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        out.push_str(&format!("{t}: {rows:?}\n"));
    }
    out
}

/// A store with an order, a fill and its counters, closed after a whole open. It is then left as a kill between two
/// steps of the first open leaves it: every step from the activity totals on undone (its rows and its marker gone,
/// as a rolled-back transaction leaves them), the earlier ones done. The next open does them again and the store is
/// what a whole open made.
#[tokio::test]
async fn a_first_open_ended_between_two_steps_is_completed_by_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    {
        let c = Ctx::with(Harness::new(open_writer(&path, "testnet-10").unwrap(), None));
        let a = ask(MAKER_A, P250);
        let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
        let leg = kob_protocol::build::Leg::Ask { order: c.w.order(&create, 0, a), custody, amount: 4 * WHOLE, t: None };
        let fill = c.w.sign(&kob_protocol::build::Action::Batch(batch(&c.w, c.daa(), vec![leg])));
        c.push(&[&fill]).await;
        assert_eq!(c.hs.status(&cov), "partial");
    }
    let whole = derived(&open_writer(&path, "testnet-10").unwrap());
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(
            "DELETE FROM activity; DELETE FROM actors; DELETE FROM kcc23_auto_live; DELETE FROM book_live; DELETE FROM pair_live; \
             DELETE FROM pair_volume; \
             DELETE FROM meta WHERE k IN ('activity', 'kcc23_auto_live', 'book_live', 'pair_volume', 'book_live_rules');",
        )
        .unwrap();
    }
    let again = derived(&open_writer(&path, "testnet-10").unwrap());
    assert_eq!(again, whole);
}
