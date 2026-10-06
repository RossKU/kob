//! If-done exits served to clients: every exit kind (buy-first `KobIfdBid` -> `KobCondAsk`, sell-first `KobIfdAsk` ->
//! `KobCondBid`, plain and repeating, KCC-20 and KRON) carries in its `GET /v1/orders/{id}` view what a client needs to spend it
//! without any other lookup: its entry's extension commitment and the proven token state of its custody and strays. A cancel is
//! built from the view alone and validated by the script engine; the matcher's listed order of an exit carries the same custody
//! state (a keeper refund of it validates). A database written before the fix (exits stored without a commitment) is repaired on
//! open to exactly the rows a fresh build gives.

mod common;

use common::*;
use kob_executor::hex::{self, Hash32};
use kob_executor::indexer::reads::{OrderView, TokenUtxoView};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;

const FAMS: [Family; 2] = [Family::Kcc20, Family::Kron];

fn ib_for(fam: Family, i: IfdBidState) -> IfdBidState {
    match fam {
        Family::Kron => match kron(AnyState::KobIfdBid(i)) {
            AnyState::KobIfdBidKron(x) => x,
            _ => unreachable!(),
        },
        _ => i,
    }
}

fn ia_for(fam: Family, i: IfdAskState) -> IfdAskState {
    match fam {
        Family::Kron => match kron(AnyState::KobIfdAsk(i)) {
            AnyState::KobIfdAskKron(x) => x,
            _ => unreachable!(),
        },
        _ => i,
    }
}

/// The extension commitment a view of an order of the fixtures' token carries (KRON: none, stored as zero).
fn ext_hex(fam: Family) -> String {
    hex::encode(&if fam == Family::Kron { [0; 32] } else { EXT })
}

/// An exit booked by a fill of 4 whole tokens of a fresh entry (`repeat`: a repeating entry, the exit is booked to it), with a stray of
/// 2 whole tokens sent to it afterwards. Returns (exit covenant id, entry covenant id, the fill).
async fn booked_exit(c: &Ctx, fam: Family, buy_first: bool, repeat: bool) -> (Hash32, Hash32, SignedTx) {
    let rpt_amount = if repeat { 1 + 20 * WHOLE } else { 0 };
    let (create, fill) = if buy_first {
        let ib = ib_for(fam, IfdBidState { rpt_amount, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) });
        let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()).into_family(fam), ib.escrow().unwrap() as u64, MAKER_A, 0);
        c.push(&[&create]).await;
        let mut b =
            batch(&c.w, c.daa(), vec![Leg::IfdBid { order: c.w.order(&create, 0, ib), amount: 4 * WHOLE, evidence: None, t: None }]);
        b.taker_tokens = vec![c.w.token_for(fam, TAKER, 4 * WHOLE)];
        (create.clone(), c.w.sign(&Action::Batch(b)))
    } else {
        let ia = ia_for(fam, IfdAskState { rpt_amount, min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) });
        let create = c.w.create_tx(
            AnyState::KobIfdAsk(ia.clone()).into_family(fam),
            ia.escrow(CARRIER as i64).unwrap() as u64,
            MAKER_A,
            10 * WHOLE,
        );
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let custody = c.w.token_at(&create, 1, TokenState::custody(fam, 10 * WHOLE, cov.0, EXT));
        let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia), custody, amount: 4 * WHOLE, evidence: None, t: None };
        (create.clone(), c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg]))))
    };
    c.push(&[&fill]).await;
    let entry = c.w.cov(&create, 0);
    let (_, exit) = find_fresh(&fill, &[entry]).expect("the exit");
    let parent: Vec<u8> = c.hs.query("SELECT parent FROM orders WHERE covenant_id = ?1", [&exit.0[..]]);
    assert_eq!(parent, entry.0.to_vec());
    let stray = c.w.stray_for(fam, exit, 2 * WHOLE, MAKER_B);
    c.push(&[&stray]).await;
    (exit, entry, fill)
}

fn h32(s: &str) -> [u8; 32] {
    *Hash32::parse(s).expect("32-byte hex").as_bytes()
}

/// A token UTXO exactly as a client takes it from a view: outpoint, carrier and the proven state the view serves.
fn token_from_view(v: &TokenUtxoView) -> TokenUtxo {
    let state = v.state.clone().unwrap_or_else(|| panic!("token UTXO {}:{} has no proven state in the view", v.txid, v.index));
    TokenUtxo {
        utxo: Utxo {
            transaction_id: h32(&v.txid),
            index: v.index as u32,
            amount: v.value.parse().unwrap(),
            block_daa_score: v.created_daa as u64,
            covenant_id: Some(h32(&v.token)),
        },
        state: serde_json::from_value(state).expect("the served token state is the builders' JSON"),
    }
}

/// The cancel of an order built from nothing but its indexer view (what the web client and the soak bots do).
fn cancel_from_view(c: &Ctx, v: &OrderView) -> SignedTx {
    assert!(v.state_known, "{}: state not proven", v.covenant_id);
    let cur = v.current.as_ref().expect("a live order has a current outpoint");
    let state: AnyState = serde_json::from_value(v.state.clone().expect("state")).expect("the served order state decodes");
    let order = OrderUtxo {
        utxo: Utxo {
            transaction_id: h32(&cur.txid),
            index: cur.index as u32,
            amount: cur.value.as_deref().expect("carrier").parse().unwrap(),
            block_daa_score: v.current_daa.expect("utxo daa") as u64,
            covenant_id: Some(h32(&v.covenant_id)),
        },
        state,
    };
    let custody = v.custody.as_ref().and_then(|cv| cv.utxo.as_ref()).map(token_from_view);
    let strays = v.strays.as_deref().unwrap_or_default().iter().map(token_from_view).collect();
    c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order,
        custody,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

#[tokio::test]
async fn every_exit_view_carries_its_extension_commitment_and_proven_token_states_and_cancels_from_the_view_alone() {
    for fam in FAMS {
        for buy_first in [true, false] {
            for repeat in [false, true] {
                let label = format!(
                    "{fam:?} {} {}",
                    if buy_first { "buy-first" } else { "sell-first" },
                    if repeat { "repeat" } else { "plain" }
                );
                let c = Ctx::new();
                let (exit, entry, _) = booked_exit(&c, fam, buy_first, repeat).await;
                let v = view(&c, &exit);
                let kind = if buy_first { "KobCondAsk" } else { "KobCondBid" };
                let kind = if fam == Family::Kron { format!("{kind}Kron") } else { kind.to_string() };
                assert_eq!(v.contract, kind, "{label}");
                assert_eq!(v.extension_commitment.as_deref(), Some(ext_hex(fam).as_str()), "{label}: the exit's commitment");
                assert_eq!(
                    v.extension_commitment,
                    view(&c, &entry).extension_commitment,
                    "{label}: the exit's commitment is its entry's"
                );
                assert_eq!(v.repeat.as_ref().and_then(|r| r.parent.clone()), repeat.then(|| entry.to_hex()), "{label}");
                if buy_first {
                    // the custody: the bought amount, with the state the indexer proved (owner = the exit, the entry's commitment)
                    let cv = v.custody.as_ref().expect("ask-side exit");
                    assert!(cv.ok, "{label}: {cv:?}");
                    let cu = cv.utxo.as_ref().expect("custody utxo");
                    let st: TokenState = serde_json::from_value(cu.state.clone().expect("proven custody state")).unwrap();
                    assert_eq!(st, TokenState::custody(fam, 4 * WHOLE, exit.0, EXT), "{label}");
                } else {
                    assert!(v.custody.is_none(), "{label}: a bid-side exit holds KAS");
                }
                let strays = v.strays.as_deref().unwrap();
                assert_eq!(strays.len(), 1, "{label}");
                let st: TokenState = serde_json::from_value(strays[0].state.clone().expect("proven stray state")).unwrap();
                assert_eq!(st, TokenState::custody(fam, 2 * WHOLE, exit.0, EXT), "{label}");
                // `/v1/strays` serves the same state
                {
                    let g = c.hs.ingest.lock().unwrap();
                    let ctx = kob_executor::indexer::reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
                    let all = kob_executor::indexer::reads::strays(g.conn(), &ctx, None, 10).unwrap();
                    assert_eq!(all.len(), 1, "{label}");
                    assert_eq!(all[0].utxo.state, strays[0].state, "{label}");
                }

                // the maker cancels from the view alone: validated by the script engine, custody and stray swept
                let cancel = cancel_from_view(&c, &v);
                c.push(&[&cancel]).await;
                assert_eq!(c.hs.status(&exit), "cancelled", "{label}");
                assert_eq!(
                    c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&exit.0[..]]),
                    0,
                    "{label}: the cancel swept every token of the exit"
                );
                // the entry is untouched and still cancellable from its own view
                let cancel = cancel_from_view(&c, &view(&c, &entry));
                c.push(&[&cancel]).await;
                assert_eq!(c.hs.status(&entry), "cancelled", "{label}");
            }
        }
    }
}

#[tokio::test]
async fn the_matchers_listed_exit_carries_the_entrys_commitment_and_a_keeper_refund_of_it_validates() {
    for fam in FAMS {
        for repeat in [false, true] {
            let c = Ctx::new();
            let (exit, _, _) = booked_exit(&c, fam, true, repeat).await;
            let listed = {
                let g = c.hs.ingest.lock().unwrap();
                kob_executor::indexer::book::listed_orders(g.conn()).unwrap()
            };
            let lo = listed.into_iter().find(|o| o.order.utxo.covenant_id == Some(exit.0)).expect("the exit is listed");
            let custody = lo.custody.clone().expect("the exit's custody");
            assert_eq!(custody.state, TokenState::custody(fam, 4 * WHOLE, exit.0, EXT), "{fam:?} repeat {repeat}");
            assert_eq!(lo.extension(), Some(if fam == Family::Kron { [0; 32] } else { EXT }));
            assert!(lo.custody_ok(), "{fam:?} repeat {repeat}");
            assert_eq!(lo.strays.len(), 1);
            assert_eq!(lo.strays[0].state, TokenState::custody(fam, 2 * WHOLE, exit.0, EXT));
            // a refund keeper spends the custody with that state
            let due = lo.order.state.refund_due(lo.order.utxo.block_daa_score as i64).expect("refund due");
            let refund = c.w.sign(&Action::RefundOrder(RefundOrder {
                prefund: None,
                order: lo.order.clone(),
                foreign: vec![],
                custody: Some(custody),
                lock_time: due as u64,
                funding: vec![c.w.coin(KEEPER, 10)],
                change: Some(pk(KEEPER)),
                fee: FeeOptions::default(),
            }));
            c.push(&[&refund]).await;
            assert_eq!(c.hs.status(&exit), "refunded", "{fam:?} repeat {repeat}");
        }
    }
}

/// A database written by a build before the fix: the exits stored without an extension commitment and the `seen` token
/// registry event such an exit recorded for the (token, program, no commitment) identity. Opening it repairs both: the rows
/// are then exactly those of a fresh build, and the exit's view cancels.
#[tokio::test]
async fn a_database_with_exits_stored_without_a_commitment_is_repaired_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.sqlite3");
    let c = Ctx::with(Harness::new(kob_executor::indexer::db::open_writer(&path, "testnet-10").unwrap(), None));
    let (exit_a, _, fill) = booked_exit(&c, Family::Kcc20, true, true).await;
    let (exit_b, _, _) = booked_exit(&c, Family::Kcc20, false, false).await;
    let (exit_c, _, _) = booked_exit(&c, Family::Kron, true, false).await;
    let fresh = fresh_snapshot(&c.hs.node.chain());
    assert_eq!(c.hs.snapshot(), fresh);

    // the rows the earlier build wrote
    {
        let g = c.hs.ingest.lock().unwrap();
        let conn = g.conn();
        conn.execute(
            "UPDATE orders SET ext_commit = NULL WHERE contract IN ('KobCondAsk', 'KobCondAskKron') AND parent IS NOT NULL",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) \
             SELECT token_cov_id, token_tpl_hash, NULL, 'seen', genesis_txid, genesis_block, genesis_daa FROM orders \
             WHERE covenant_id IN (?1, ?2)",
            [&exit_a.0[..], &exit_c.0[..]],
        )
        .unwrap();
        let genesis: Vec<u8> =
            conn.query_row("SELECT genesis_txid FROM orders WHERE covenant_id = ?1", [&exit_a.0[..]], |r| r.get(0)).unwrap();
        assert_eq!(genesis, fill.tx.id.to_vec());
    }
    assert_ne!(c.hs.snapshot(), fresh);
    let old = view(&c, &exit_a);
    assert_eq!(old.extension_commitment, None, "the bug: an exit view without its commitment");
    assert_eq!(
        view(&c, &exit_b).extension_commitment.as_deref(),
        Some(ext_hex(Family::Kcc20).as_str()),
        "a KobCondBid exit's is its state's"
    );

    // restart: the writer opens the database again
    drop(kob_executor::indexer::db::open_writer(&path, "testnet-10").unwrap());
    assert_eq!(c.hs.snapshot(), fresh, "repaired to the rows of a fresh build");
    for x in [exit_a, exit_c] {
        let v = view(&c, &x);
        assert!(v.extension_commitment.is_some());
        let cancel = cancel_from_view(&c, &v);
        c.push(&[&cancel]).await;
        assert_eq!(c.hs.status(&x), "cancelled");
    }
    // idempotent: nothing left to repair
    let g = c.hs.ingest.lock().unwrap();
    assert_eq!(kob_executor::indexer::db::backfill_exit_extensions(g.conn()).unwrap(), (0, 0));
}
