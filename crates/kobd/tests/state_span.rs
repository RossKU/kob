//! kobd never lists or plans an order whose state span is not byte for byte the canonical pushes of its template.
//!
//! The contracts read their fields by offset. A span with opcodes in a field's bytes, of the same length, runs them
//! (a hand-built order of the current generation forged touch evidence that way in the engine). kobd's three ways
//! in for a state all refuse such a span:
//! - a placement record spells the fields, and the decoder rebuilds the canonical span from them, so the record can
//!   only name the canonical state, and an output scripted with the rigged span is not its P2SH (refused);
//! - a reveal and an import decode with `AnyState::decode`, which re-encodes and requires the same bytes.

mod common;

use common::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::state::*;
use kobd::hex::HexBytes;
use kobd::testkit::*;

/// The ask state's span with the push of its `maker` key replaced by opcodes of the same length: a push of 31 bytes
/// and an `OP_NOP` where the key's last byte was.
fn rigged_span(s: &AnyState, key: &[u8; 32]) -> Vec<u8> {
    let mut b = s.encode();
    let at = b.windows(32).position(|w| w == key).expect("the maker's key in the span");
    assert_eq!(b[at - 1], 0x20, "pushed by OP_DATA_32");
    b[at - 1] = 0x1f;
    b[at + 31] = 0x61;
    b
}

#[test]
fn a_rigged_span_does_not_decode() {
    let s = AnyState::KobAsk(ask(MAKER_A, P250));
    let b = rigged_span(&s, &pk(MAKER_A));
    assert_eq!(b.len(), s.encode().len(), "same length");
    let e = AnyState::decode(TemplateId::KobAsk, &b).expect_err("refused");
    assert!(!e.to_string().is_empty());
}

/// An order output scripted with the rigged span next to the canonical placement record is no order: refused at the
/// placement, nothing listed.
#[tokio::test]
async fn an_order_output_scripted_with_a_rigged_span_is_never_listed() {
    let c = Ctx::new();
    let state = AnyState::KobAsk(ask(MAKER_A, P250));
    let create = c.w.create_tx(state.clone(), CARRIER, MAKER_A, 10 * WHOLE);
    let cov = c.w.cov(&create, 0);
    let tpl = template(TemplateId::KobAsk);
    let redeem = [tpl.prefix.clone(), rigged_span(&state, &pk(MAKER_A)), tpl.suffix.clone()].concat();
    let mut w = wire_tx(&create.tx);
    w.outputs[0].script_public_key = HexBytes(kobd::script::p2sh_spk(&redeem));
    c.w.note_daa(&[&create], c.hs.node.tip_daa() + 1);
    c.w.include_with(&c.hs.node, &[], vec![w]);
    c.hs.sync().await;
    let g = c.hs.ingest.lock().unwrap();
    let orders: i64 = g.conn().query_row("SELECT COUNT(*) FROM orders WHERE covenant_id = ?1", [&cov.0[..]], |r| r.get(0)).unwrap();
    assert_eq!(orders, 0, "never an order");
    let rejects: Vec<String> =
        g.conn().prepare("SELECT reason FROM rejects").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    assert!(rejects.iter().any(|r| r.starts_with("placement:")), "refused at the placement: {rejects:?}");
}
