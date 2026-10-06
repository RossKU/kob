//! C5 liveness audit, recovery: an order the gap reconciliation closed cannot be re-imported.
//!
//! `index rebase` (`recover::reconcile_open_orders`) closes every tracked order UTXO the node no longer has at the SAME
//! script (`LogOp::CloseGap`, status `closed`, its custody rows marked spent) and only adopts a successor with the same script
//! (practically only bids: every ask-side fill changes `amountLeft` and so the script). An ask that was partially filled inside
//! the gap is therefore closed although it lives on under a new script. `docs/ops/executor.md` 7.4 / 7.5 then send the maker
//! to `import-orders`, but `Processor::apply_op(LogOp::Import)` goes through `create_order`, which answers
//! `duplicate_covenant` for any known covenant id (`order_exists`) and is reported as `already_known`: the order stays `closed`,
//! has no current outpoint and no custody, is not listed (matcher / keepers) and cannot be cancelled from the view.
//! (The CLI `recover` command the doc names is not implemented: `kob-cli` lists it as "Planned".)

mod common;

use common::*;
use kob_executor::hex::{Hash32, HexBytes};
use kob_executor::indexer::ingest::Cursor;
use kob_executor::recover::{import_orders, reconcile_open_orders, ExportedOrder, MakerExport};
use kob_executor::rpc::types::{AddressUtxo, AddressUtxoEntry, Outpoint};
use kob_executor::script::{spk_address, spk_bytes};
use kob_executor::testkit::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;

/// An unspent output as the node's `getUtxosByAddresses` reports it.
fn node_utxo(spk: &[u8], cov: Hash32, txid: Hash32, idx: u32, amount: u64) -> (String, AddressUtxo) {
    (
        spk_address(spk, "testnet-10").unwrap(),
        AddressUtxo {
            outpoint: Outpoint { transaction_id: txid, index: idx },
            utxo_entry: AddressUtxoEntry {
                amount,
                script_public_key: HexBytes(spk.to_vec()),
                block_daa_score: 1_050,
                covenant_id: Some(cov),
            },
        },
    )
}

#[tokio::test]
async fn an_ask_partially_filled_in_the_gap_can_be_imported_by_its_maker_after_the_rebase() {
    // C5: expected to fail until `LogOp::Import` revives a known order whose every UTXO was closed by the gap (insert the
    // verified UTXO and custody, event `update`/`import`, refresh the state) instead of answering `already_known`.
    let c = Ctx::new();
    let a = ask(MAKER_A, P250); // 10 whole tokens
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");

    // During the indexer's downtime a taker fills 4 whole tokens: the transaction is on the node's chain, never seen by the indexer
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let oi = find_cov(&fill, &cov).unwrap();
    let ci = find_custody(&fill, &cov, 6 * WHOLE).unwrap();
    let order_spk = spk_bytes(&AnyState::KobAsk(a6.clone()).spk());
    let custody_spk = spk_bytes(&Kcc20State::custody(6 * WHOLE, cov.0, EXT).spk_with(template(T3)));
    c.hs.node.set_utxos(vec![
        node_utxo(&order_spk, cov, Hash32(fill.tx.id), oi as u32, fill.tx.outputs[oi].value),
        node_utxo(&custody_spk, Hash32(TOKEN_COV), Hash32(fill.tx.id), ci as u32, fill.tx.outputs[ci].value),
    ]);

    // `index rebase`: the old script has no UTXO any more and no successor with the same script: closed by the gap
    let cursor = Cursor { hash: c.hs.node.tip(), daa: c.daa() };
    let rep = reconcile_open_orders(&c.hs.ingest, c.hs.node.as_ref(), "testnet-10", cursor).await.unwrap();
    assert_eq!((rep.closed, rep.adopted), (1, 0), "{rep:?}");
    assert_eq!(c.hs.status(&cov), "closed");

    // the maker imports the order from the receipt `export-orders` gave them (current state, custody extension commitment)
    let export = MakerExport {
        version: 2,
        network: "testnet-10".into(),
        orders: vec![ExportedOrder {
            template_hash: Hash32(template(TemplateId::KobAsk).hash),
            state: HexBytes(AnyState::KobAsk(a6.clone()).encode()),
            extension_commitment: Some(Hash32(EXT)),
            prefund_extension_commitment: None,
            covenant_id: Some(cov),
            genesis_txid: None,
            status: None,
            parent: None,
        }],
    };
    let rep = import_orders(&c.hs.ingest, c.hs.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!(rep.imported, 1, "the live order was reported as already known and stays closed: {rep:?}");
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a6)));
    let v = view(&c, &cov);
    assert!(v.current.is_some(), "a current outpoint to cancel");
    let cv = v.custody.expect("ask-side view");
    assert!(cv.ok && cv.utxo.is_some(), "custody verified against the node: {cv:?}");
}

#[tokio::test]
async fn an_adopted_ask_keeps_its_custody_and_live_strays_and_takes_the_node_daa() {
    // K-2: a same-script successor is adopted after a gap. Its custody is looked up at the script the state derives, the
    // strays the node still holds stay live, and the stored UTXO DAA is the node's (the refund time counts from it), not the
    // reconciliation cursor's.
    let c = Ctx::new();
    let a = ask(MAKER_A, P250); // 10 whole tokens
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let s1 = c.w.stray(cov, 2 * WHOLE, TAKER);
    let s2 = c.w.stray(cov, 3 * WHOLE, TAKER);
    c.push(&[&s1, &s2]).await;
    assert_eq!(view(&c, &cov).strays.unwrap().len(), 2);
    let stray_out = |t: &kob_protocol::tx::SignedTx, amount: i64| find_custody(t, &cov, amount).unwrap();
    let (i1, i2) = (stray_out(&s1, 2 * WHOLE), stray_out(&s2, 3 * WHOLE));

    // during the gap the order continued under the SAME script at a new outpoint with a new custody; the first stray is
    // still unspent, the second is gone
    let order_spk = spk_bytes(&AnyState::KobAsk(a.clone()).spk());
    let custody_spk = spk_bytes(&Kcc20State::custody(10 * WHOLE, cov.0, EXT).spk_with(template(T3)));
    let s1_spk = spk_bytes(&Kcc20State::custody(2 * WHOLE, cov.0, EXT).spk_with(template(T3)));
    let mut stray_left = node_utxo(&s1_spk, Hash32(TOKEN_COV), Hash32(s1.tx.id), i1 as u32, s1.tx.outputs[i1].value);
    stray_left.1.utxo_entry.block_daa_score = 900;
    c.hs.node.set_utxos(vec![
        node_utxo(&order_spk, cov, Hash32([0x77; 32]), 0, CARRIER),
        node_utxo(&custody_spk, Hash32(TOKEN_COV), Hash32([0x78; 32]), 1, CARRIER),
        stray_left,
    ]);
    let cursor = Cursor { hash: c.hs.node.tip(), daa: c.daa() + 5_000 };
    let rep = reconcile_open_orders(&c.hs.ingest, c.hs.node.as_ref(), "testnet-10", cursor).await.unwrap();
    assert_eq!((rep.closed, rep.adopted), (0, 1), "{rep:?}");
    assert_eq!(c.hs.status(&cov), "open");

    let v = view(&c, &cov);
    let cur = v.current.clone().expect("adopted output");
    assert_eq!(cur.txid, Hash32([0x77; 32]).to_hex());
    let cv = v.custody.clone().expect("ask-side view");
    assert!(cv.ok, "the adopted ask has its custody: {cv:?}");
    let cu = cv.utxo.expect("custody utxo");
    assert_eq!((cu.txid.as_str(), cu.index), (Hash32([0x78; 32]).to_hex().as_str(), 1));
    assert!(cu.state.is_some(), "the custody carries its proven token state (cancel builds from the view)");
    let strays = v.strays.unwrap();
    assert_eq!(strays.len(), 1, "{strays:?}");
    assert_eq!((strays[0].txid.as_str(), strays[0].index), (Hash32(s1.tx.id).to_hex().as_str(), i1 as i64));
    let _ = i2;

    // the stored UTXO DAA is the node's block DAA of the adopted output (1_050), not the cursor
    let daa: i64 = {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .query_row("SELECT created_daa FROM order_utxos WHERE txid = ?1 AND idx = 0", [&[0x77u8; 32][..]], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(daa, 1_050);
}

#[tokio::test]
async fn an_imported_exit_is_linked_to_its_known_entry() {
    // K-7: an export carries an if-done exit's entry; the import links them (the entry's view lists the exit as a child)
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let entry = c.w.cov(&create, 0);
    // an "exit" order the indexer never saw, imported with the entry as its parent (any order kind links the same way)
    let exit_state = AnyState::KobAsk(AskState { amount_left: 3 * WHOLE, ..ask(MAKER_A, P250 + 1) });
    let exit_spk = spk_bytes(&exit_state.spk());
    let exit_cov = Hash32([0x55; 32]);
    let custody_spk = spk_bytes(&Kcc20State::custody(3 * WHOLE, exit_cov.0, EXT).spk_with(template(T3)));
    c.hs.node.set_utxos(vec![
        node_utxo(&exit_spk, exit_cov, Hash32([0x56; 32]), 0, CARRIER),
        node_utxo(&custody_spk, Hash32(TOKEN_COV), Hash32([0x56; 32]), 1, CARRIER),
    ]);
    let export = MakerExport {
        version: 2,
        network: "testnet-10".into(),
        orders: vec![ExportedOrder {
            template_hash: Hash32(template(TemplateId::KobAsk).hash),
            state: HexBytes(exit_state.encode()),
            extension_commitment: Some(Hash32(EXT)),
            prefund_extension_commitment: None,
            covenant_id: Some(exit_cov),
            genesis_txid: None,
            status: None,
            parent: Some(entry),
        }],
    };
    let rep = import_orders(&c.hs.ingest, c.hs.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!(rep.imported, 1, "{rep:?}");
    assert!(rep.custody_unverified.is_empty(), "{rep:?}");
    let ev = view(&c, &entry);
    assert_eq!(ev.children.clone().unwrap(), vec![exit_cov.to_hex()]);
    let xv = view(&c, &exit_cov);
    assert_eq!(xv.parent.as_deref(), Some(entry.to_hex().as_str()));
    let cu = xv.custody.unwrap().utxo.expect("custody");
    assert!(cu.state.is_some(), "an imported order has its custody's holding row (proven state)");
}
