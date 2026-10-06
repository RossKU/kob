//! C6 regressions (findings of the C6 fuzzers, `research/2026-09-29/planning/core/c6_unknowns.md`).
//!
//! * C6-1: [`kob_protocol::tx::masses`] (and so `verify::validate`, `tx::finalize`, kob-wasm `masses` / `validate` /
//!   `finalize`, the x402 verifiers' mass check) panicked with "attempt to divide by zero" inside rusty-kaspa's
//!   KIP-9 storage-mass calculator on a transaction without inputs or with a zero-value output. Consensus refuses such a
//!   transaction before it computes masses; the library must refuse it too, without a panic: its storage mass is
//!   reported as beyond every limit.

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{
    ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kob_protocol::script::p2pk_spk;
use kob_protocol::tx::masses;
use kob_protocol::verify::validate;

fn key() -> [u8; 32] {
    kob_protocol::tx::pubkey_of(&[7; 32]).unwrap()
}

fn input(tag: u8) -> TransactionInput {
    TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([tag; 32]), index: 0 },
        vec![0x51],
        0,
        0,
    )
}

fn entry(amount: u64) -> UtxoEntry {
    UtxoEntry::new(amount, p2pk_spk(&key()), 1_000, false, None)
}

fn output(value: u64) -> TransactionOutput {
    TransactionOutput { value, script_public_key: p2pk_spk(&key()), covenant: None }
}

fn tx(inputs: Vec<TransactionInput>, outputs: Vec<TransactionOutput>) -> Transaction {
    Transaction::new(1, inputs, outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![])
}

#[test]
fn c6_1_masses_of_a_transaction_without_inputs_is_an_error_not_a_panic() {
    let t = tx(vec![], vec![output(100_000_000)]);
    let m = masses(&t, &[]);
    assert_eq!(m.storage, u64::MAX);
    assert!(!m.within_block_limits());
    assert!(validate(&t, &[]).is_err());
}

#[test]
fn c6_1_masses_of_a_zero_value_output_is_an_error_not_a_panic() {
    for outs in [vec![output(0)], vec![output(100_000_000), output(0)], vec![output(0), output(0), output(5)]] {
        let ins: Vec<TransactionInput> = (1..=3).map(input).collect();
        let t = tx(ins, outs);
        let en = vec![entry(1_000_000_000); 3];
        let m = masses(&t, &en);
        assert_eq!(m.storage, u64::MAX);
        assert!(!m.within_block_limits());
        assert!(validate(&t, &en).is_err());
    }
}

#[test]
fn c6_1_masses_with_missing_entries_is_an_error_not_a_panic() {
    let t = tx(vec![input(1), input(2)], vec![output(100_000_000)]);
    let m = masses(&t, &[entry(1_000_000_000)]);
    assert_eq!(m.storage, u64::MAX);
}

#[test]
fn c6_1_masses_of_a_valid_transaction_are_unchanged() {
    let t = tx(vec![input(1)], vec![output(500_000_000), output(400_000_000)]);
    let m = masses(&t, &[entry(1_000_000_000)]);
    assert!(m.storage > 0 && m.storage < u64::MAX);
    let _ = ScriptPublicKey::default();
}

/// Every order state span that decodes re-encodes to the same bytes (a decodable span the builders cannot re-encode
/// would make them panic, or build a script different from the order's own). Spans: valid encodings of every kind
/// with one 8-byte number window overwritten by random bytes, including the sign bit (negative zero, `i64::MIN`
/// patterns).
#[test]
fn c6_decoded_order_states_re_encode_canonically() {
    use kob_protocol::artifacts::TemplateId;
    use kob_protocol::state::AnyState;
    let mut seed: u64 = 0xc6c6_0001;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let kinds = TemplateId::ALL.into_iter().filter(|t| t.kind_code().is_some()).collect::<Vec<_>>();
    let (mut decoded, mut differ) = (0, vec![]);
    for _ in 0..4_000 {
        let t = kinds[(next() % kinds.len() as u64) as usize];
        let tpl = kob_protocol::artifacts::template(t);
        let mut span = vec![0u8; tpl.state_len];
        // a valid encoding to start from: decode the artifact's example instance span
        let ex = &tpl.artifact.contracts[&tpl.contract_name].compiled.bytecode;
        span.copy_from_slice(&ex[tpl.prefix.len()..tpl.prefix.len() + tpl.state_len]);
        // an 8-byte push (0x08 + 8 bytes) somewhere in the span: overwrite its value
        let pushes: Vec<usize> = (0..span.len().saturating_sub(9)).filter(|&i| span[i] == 0x08).collect();
        if pushes.is_empty() {
            continue;
        }
        let at = pushes[(next() % pushes.len() as u64) as usize] + 1;
        let v = next().to_le_bytes();
        span[at..at + 8].copy_from_slice(&v);
        if next() % 3 == 0 {
            span[at..at + 7].fill(0);
            span[at + 7] = 0x80; // negative zero
        }
        let Ok(s) = AnyState::decode(t, &span) else { continue };
        decoded += 1;
        match s.try_encode() {
            Ok(e) if e == span => {}
            Ok(_) => differ.push(format!("{}: window at {at} re-encodes differently: {:?}", t.name(), &span[at..at + 8])),
            Err(e) => differ.push(format!("{}: window at {at}: decodes but does not re-encode: {e}", t.name())),
        }
    }
    println!("{decoded} spans decoded, {} not canonical", differ.len());
    for d in differ.iter().take(10) {
        println!("  {d}");
    }
    assert!(decoded > 1_000);
    assert!(differ.is_empty(), "{} decodable spans do not re-encode to themselves", differ.len());
}
