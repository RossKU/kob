//! The builder's side of the terminal-bid refund (`contracts/orders/KobBid.sil` `refundTo`): the KAS a terminal bid fill does not
//! spend beyond its delivery carrier returns to the maker as a plain P2PK output at output `M - 1 - i`; the legs with a refund
//! are put first; a remainder below `REFUND_MIN` stays on the delivery. Every transaction is validated in the engine.

mod common;

use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action, Batch, Leg};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, KeyUtxo, OrderUtxo, TokenUtxo, Utxo, REFUND_MIN};
use kob_protocol::verify::validate_signed;
use kob_protocol::Family;

const T: TemplateId = TemplateId::Kcc20Ref;
const CARRIER: u64 = 200_000_000;
const FILL: i64 = 20 * WHOLE;

fn pu(kind: u8, j: usize, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
    let mut id = [0x5c; 32];
    id[0] = kind;
    id[1] = j as u8;
    Utxo { transaction_id: id, index: j as u32, amount, block_daa_score: 1_000, covenant_id: cov }
}

/// An IOC bid of maker `maker` funded for `FILL` base units plus `extra` sompi of spare budget, filled for `FILL`.
fn ioc_bid(j: usize, maker: u8, extra: i64) -> (Leg, BidState, i64) {
    let mut s = BidState { tif: 1, ..bid(maker, 240_000_000, T) };
    s.min_fill = WHOLE;
    s.delivery_carrier = CARRIER as i64;
    let spend = s.spend(FILL, 0, 1_000).unwrap();
    let value = spend + CARRIER as i64 + extra;
    let mut c = [0xc7; 32];
    c[0] = 0xb7;
    c[1] = j as u8;
    let leg = Leg::Bid { order: OrderUtxo { utxo: pu(0xb7, j, value as u64, Some(c)), state: s.clone() }, amount: FILL, t: None };
    (leg, s, spend)
}

fn batch_of(legs: Vec<Leg>, bids: usize) -> Batch {
    Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![TokenUtxo {
            utxo: pu(0xb8, 0, CARRIER, Some(TOKEN_COV)),
            state: TokenState::user(Family::Kcc20, bids as i64 * FILL, pk(TAKER), ext_for(T)),
        }],
        taker: Some(pk(TAKER)),
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![KeyUtxo { utxo: pu(0xb9, 0, 1_000 * KAS, None), pubkey: pk(TAKER) }],
        change: Some(pk(TAKER)),
        records: vec![],
        fee: fee(),
    }
}

/// The validated transaction and the builder's change output index.
fn run(b: Batch) -> Result<(kaspa_consensus_core::tx::Transaction, Option<u32>), String> {
    let built = with_carriers(Carriers::order_side(CARRIER), || build(&Action::Batch(b))).map_err(|e| e.to_string())?;
    let sigs = sign_locally(&built, &keys()).map_err(|e| e.to_string())?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    validate_signed(&signed).map_err(|e| format!("engine: {e}"))?;
    let (tx, _) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    Ok((tx, built.fee.change_output))
}

#[test]
fn terminal_fills_return_the_spare_kas_as_the_last_outputs() {
    let extras = [3_000_000_000i64, 150_000_000];
    let bids: Vec<_> = extras.iter().enumerate().map(|(j, e)| ioc_bid(j, 20 + j as u8, *e)).collect();
    let (tx, change) = run(batch_of(bids.iter().map(|b| b.0.clone()).collect(), 2)).expect("honest layout");
    let outs = &tx.outputs;
    let m = outs.len();
    for (i, (_, s, _)) in bids.iter().enumerate() {
        // delivery i: only the carrier; refund of input i at M - 1 - i: the spare budget to P2PK(maker)
        assert_eq!(outs[i].value, CARRIER, "delivery {i}");
        let r = &outs[m - 1 - i];
        assert_eq!(r.script_public_key, p2pk_spk(&s.maker), "refund {i} goes to the maker");
        assert_eq!(r.value as i64, extras[i], "refund {i} is the whole spare budget");
        assert!(r.covenant.is_none(), "a plain output");
    }
    // the change sits before the refunds
    assert_eq!(change, Some((m - 3) as u32));
}

#[test]
fn the_threshold_is_one_kas_beyond_the_carrier() {
    // exactly REFUND_MIN: refund; one sompi less: it stays on the delivery
    let (leg, s, _) = ioc_bid(0, 20, REFUND_MIN as i64);
    let (tx, _) = run(batch_of(vec![leg], 1)).unwrap();
    let outs = &tx.outputs;
    assert_eq!(outs[0].value, CARRIER);
    assert_eq!(outs.last().unwrap().script_public_key, p2pk_spk(&s.maker));
    assert_eq!(outs.last().unwrap().value, REFUND_MIN);

    let (leg, s, _) = ioc_bid(0, 20, REFUND_MIN as i64 - 1);
    let (tx, _) = run(batch_of(vec![leg], 1)).unwrap();
    let outs = &tx.outputs;
    assert_eq!(outs[0].value, CARRIER + REFUND_MIN - 1, "below the threshold everything rides on the delivery");
    assert!(outs.iter().all(|o| o.script_public_key != p2pk_spk(&s.maker)), "no refund output");
}

#[test]
fn legs_with_a_refund_are_put_first_and_blame_keeps_the_callers_indices() {
    // [no refund, refund]: the second bid is put first (its refund must be the last output)
    let (a, _, _) = ioc_bid(0, 20, 0);
    let (b, sb, _) = ioc_bid(1, 21, 2_000_000_000);
    let (tx, _) = run(batch_of(vec![a, b.clone()], 2)).expect("reordered");
    let Leg::Bid { order, .. } = &b else { unreachable!() };
    assert_eq!(tx.inputs[0].previous_outpoint.transaction_id.as_bytes(), order.utxo.transaction_id, "the refund leg is input 0");
    let outs = &tx.outputs;
    assert_eq!(outs.last().unwrap().script_public_key, p2pk_spk(&sb.maker));

    // a refusal names the leg by the caller's index, not by the reordered one
    let (a, _, _) = ioc_bid(0, 20, 0);
    let (mut bad, _, _) = ioc_bid(1, 21, 2_000_000_000);
    let (good, _, _) = ioc_bid(2, 22, 2_000_000_000);
    if let Leg::Bid { amount, .. } = &mut bad {
        *amount = WHOLE / 2; // below minFill
    }
    let e = run(batch_of(vec![a, bad, good], 3)).unwrap_err();
    assert!(e.contains("leg 1"), "{e}");
}
