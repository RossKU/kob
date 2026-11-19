//! A node's spent-output entries never decide how a spend of a tracked output is read.
//!
//! No hash covers the entry a node attaches to an input. A chain authority that drops its covenant
//! id or alters its script must neither hide a fill (the continuation would leave the book) nor
//! keep a cancelled order listed.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::rpc::types::SpentUtxo;
use kobd::testkit::*;

/// Includes `t` in a new chain block as a lying node reports it: `f` changes the spent entry of
/// every covenant input.
async fn push_tampered(c: &Ctx, t: &SignedTx, f: fn(&mut SpentUtxo)) {
    c.w.note_daa(&[t], c.hs.node.tip_daa() + 1);
    let mut wire = wire_tx(&t.tx);
    for u in wire.inputs.iter_mut().filter_map(|i| i.verbose_data.as_mut()?.utxo_entry.as_mut()) {
        if u.covenant_id.is_some() {
            f(u);
        }
    }
    c.hs.node.push_block(vec![wire]);
    c.hs.sync().await;
}

fn drop_covenant(u: &mut SpentUtxo) {
    u.covenant_id = None;
}

fn other_script(u: &mut SpentUtxo) {
    u.script_public_key.0 = vec![0, 0, 0xaa, 0x20, 7, 7, 7, 0x87];
}

/// A fill whose spent entries lost their covenant ids, or name another script, is still a fill:
/// the continuation is tracked, with its state.
#[tokio::test]
async fn a_fill_is_read_from_the_store_not_from_the_nodes_entry() {
    for tamper in [drop_covenant as fn(&mut SpentUtxo), other_script] {
        let c = Ctx::new();
        let a = ask(MAKER_A, P250);
        let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
        let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
        let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
        push_tampered(&c, &fill, tamper).await;
        assert_eq!(c.hs.status(&cov), "partial");
        assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
        let a6 = AskState { amount_left: 6 * WHOLE, ..a };
        assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a6)), "the continuation keeps its state");
    }
}

/// A bid's cancel has no covenant output and no KOB payload. With the covenant id dropped from its
/// spent entry it still closes the order, which is not left listed.
#[tokio::test]
async fn a_cancel_with_no_covenant_output_is_seen_without_the_nodes_covenant_id() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P245);
    let escrow = b0.escrow(10 * WHOLE, 3).unwrap() as u64;
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), escrow, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        retired: None,
        prefund: None,
        order: c.w.order(&create, 0, AnyState::KobBid(b0)),
        custody: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_B, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    assert!(cancel.tx.outputs.iter().all(|o| o.covenant.is_none()) && cancel.tx.payload.is_empty());
    push_tampered(&c, &cancel, drop_covenant).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "cancel"]);
}
