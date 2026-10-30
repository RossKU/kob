//! The maintained candle aggregates through the real write path: fills of real transactions, a reorg that takes them out, the same
//! transactions accepted again, and a rebuild from the record log all leave the charts equal to what the fills say.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::candles::{self, IVS};
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::market::{candles as chart, price_basis, scan_candles};
use kob_executor::indexer::recordlog::RecordLog;
use kob_executor::testkit::*;
use kob_executor::tokens::TokenAllowlist;
use kob_protocol::build::*;
use kob_protocol::state::*;

fn charts_equal_the_fills(c: &Ctx, token: &Hash32) {
    let g = c.hs.ingest.lock().unwrap();
    let conn = g.conn();
    let tokens = TokenAllowlist::from_entries([]);
    let basis = candles::token_basis(conn, &tokens, token).unwrap();
    assert_eq!(basis, price_basis(conn, token, None).unwrap());
    for (name, iv) in [("1m", IVS[0]), ("5m", IVS[1]), ("1h", IVS[2]), ("1d", IVS[3])] {
        let view = chart(conn, token, None, name, iv, None, None, 500).unwrap();
        let newest: Option<i64> = conn
            .query_row("SELECT MAX(ts) FROM order_events WHERE kind = 'fill' AND token_cov_id = ?1", [&token.0[..]], |r| r.get(0))
            .unwrap();
        let end = newest.map(|n| (n.div_euclid(iv) + 1) * iv).unwrap_or(0);
        let want = scan_candles(conn, token, basis, iv, end.saturating_sub(iv * 500), end, 500).unwrap();
        assert_eq!(view.items.len(), want.len(), "{name}");
        for (v, w) in view.items.iter().zip(&want) {
            assert_eq!(
                (v.t, v.o.clone(), v.h.clone(), v.l.clone(), v.c.clone()),
                (w.t, w.o.to_string(), w.h.to_string(), w.l.to_string(), w.c.to_string())
            );
            assert_eq!(
                (v.volume.clone(), v.quote_volume.clone(), v.trades),
                (w.volume.to_string(), w.quote_volume.to_string(), w.trades)
            );
        }
    }
}

fn candle_rows(c: &Ctx) -> i64 {
    c.hs.query("SELECT COUNT(*) FROM candles", [])
}

#[tokio::test]
async fn fills_reorgs_and_a_rebuild_keep_the_charts_equal_to_the_fills() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(open_memory("testnet-10").unwrap(), Some(log)));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let token = Hash32(TOKEN_COV);
    assert_eq!(candle_rows(&c), 0, "no fill, no candle");

    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(candle_rows(&c), 4, "one bucket of each of the four intervals");
    charts_equal_the_fills(&c, &token);

    // the fill's block is reorged out: its candles go with it
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(candle_rows(&c), 0, "the reorg took the buckets of its fills");
    charts_equal_the_fills(&c, &token);

    // accepted again in the next block
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    assert_eq!(candle_rows(&c), 4);
    charts_equal_the_fills(&c, &token);
    let live: Vec<String> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT iv, t, o, h, l, c, volume, quote_volume, trades FROM candles ORDER BY iv, t").unwrap();
        st.query_map([], |r| {
            Ok((0..9)
                .map(|i| r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}")).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    drop(c);

    // a database rebuilt from the record log has the same rows
    let logged = kob_executor::indexer::recordlog::read_all(dir.path()).unwrap();
    let mut ing =
        kob_executor::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    ing.init_cursor(&kob_executor::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    let replayed: Vec<String> = {
        let mut st = ing.conn().prepare("SELECT iv, t, o, h, l, c, volume, quote_volume, trades FROM candles ORDER BY iv, t").unwrap();
        st.query_map([], |r| {
            Ok((0..9)
                .map(|i| r.get::<_, rusqlite::types::Value>(i).map(|v| format!("{v:?}")).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("|"))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    assert_eq!(replayed, live);
    assert_eq!(replayed.len(), 4);
}
