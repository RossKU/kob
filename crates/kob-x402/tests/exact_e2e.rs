//! End-to-end and attack tests of the KAS standard-native flow: merchant offer -> payer payment ->
//! verification against a trusted (mock) chain -> submission -> acceptance, then one negative test per
//! verifier rule. Every accepted transaction also runs through the script engine
//! (`common::check_economics_and_scripts` inside `verify_native`).
//!
//! Replay (same transaction / same identifier twice) is a facilitator ledger concern and is tested in
//! `kob-executor`; the verifier here is pure.

mod exact_support;

use exact_support::*;
use kaspa_consensus_core::tx::{TransactionInput, TransactionOutput};
use kob_x402::chain::{ChainView, OutputStatus, SubmitError};
use kob_x402::client::native::{
    parse_payment_signature, payment_required, payment_required_header, payment_signature_header, preflight_native, revoke_native,
    select_native_offer, settlement_response, PayOptions,
};
use kob_x402::error::{Diag, Reason};
use kob_x402::safe_tx::SafeTx;
use kob_x402::testkit::{pubkey, secret};
use kob_x402::verify::PaymentKind;
use kob_x402::wire::{header_decode, header_encode, hex, Finality, Network, PaymentPayload, Resource, SettlementResponse};
use serde_json::{json, Map};

// ------------------------------------------------------------------------------------------ end to end

#[test]
fn full_flow_offer_pay_verify_submit_accept() {
    let env = Env::new();
    let coin = fund(&env.chain, PAYER, 500_000_000);
    // merchant: offer and 402 headers
    let offer = offer(AMOUNT);
    let pr = payment_required(
        Resource { url: "https://api.example.test/file".into(), description: None, mime_type: None, other: Map::new() },
        vec![offer.clone()],
        None,
    );
    let h402 = payment_required_header(&pr).unwrap();
    // payer: select, pay, preflight, header
    let got = kob_x402::client::native::parse_payment_required(&h402).unwrap();
    let chosen = select_native_offer(&got, Network::Testnet10, 30_000_000).expect("offer within the payer's limit").clone();
    assert_eq!(chosen, offer);
    let request_hash = request_hash_for(&chosen);
    let mut opts = PayOptions::new(30_000_000);
    opts.resource = Some(got.resource.clone());
    let payload =
        kob_x402::client::native::pay_native(&chosen, &request_hash, &secret(PAYER), std::slice::from_ref(&coin), NOW_MS, &opts)
            .unwrap();
    let ctx = env.ctx();
    let pre = preflight_native(&ctx, &chosen, &payload, &request_hash).expect("preflight");
    let header = payment_signature_header(&payload).unwrap();
    // merchant / facilitator: parse, verify through the dispatcher
    let received = parse_payment_signature(&header).unwrap();
    assert_eq!(received, payload);
    let v = kob_x402::verify::verify_payment(&ctx, &offer, &received, &request_hash).expect("verified");
    assert_eq!(v.kind, PaymentKind::Native);
    assert_eq!(v.txid, pre.txid);
    assert_eq!(v.amount, AMOUNT);
    assert_eq!(v.payment_output_index, 0);
    assert_eq!(v.merchant_output.script_public_key, spk_of(MERCHANT));
    assert_eq!(v.merchant_output.amount, AMOUNT);
    assert_eq!(v.merchant_output.outpoint.txid, v.tx.id().as_bytes());
    assert_eq!(v.consumed, vec![outpoint_of_coin(&coin)]);
    assert_eq!(v.finality, Finality::Accepted);
    assert_eq!(v.payer_address.as_deref(), Some(payload.payload.payer_address.as_deref().unwrap()));
    assert!(v.payment_identifier.as_deref().unwrap().starts_with("pay_"));
    assert_eq!(v.response_extension["binding"], "kaspa-exact-v2");
    assert_eq!(v.response_extension["profile"], "standard-native");
    assert_eq!(v.response_extension["paymentOutputIndex"], 0);
    assert_eq!(v.response_extension["finality"], "accepted");
    assert_eq!(v.response_extension["transactionEncoding"], "kaspa-sdk-safe-json-v2.0.0");
    assert_eq!(v.fee, 500_000_000 - v.tx.outputs.iter().map(|o| o.value).sum::<u64>());

    // the recomputed id equals what the consensus library and the spec preimage give
    assert_eq!(v.txid, kob_x402::exact::spec_txid_v0(&v.tx));

    // broadcast the exact verified transaction, mine it, observe acceptance
    let merchant_out = v.merchant_output.clone();
    assert_eq!(env.chain.output_status(&merchant_out.outpoint, &merchant_out.script_public_key).unwrap(), OutputStatus::Unknown);
    assert_eq!(env.chain.submit(&v.tx).unwrap(), v.txid);
    assert_eq!(env.chain.output_status(&merchant_out.outpoint, &merchant_out.script_public_key).unwrap(), OutputStatus::Mempool);
    env.chain.mine(0);
    assert!(matches!(
        env.chain.output_status(&merchant_out.outpoint, &merchant_out.script_public_key).unwrap(),
        OutputStatus::Accepted { .. }
    ));
    let paid = env.chain.utxo(&merchant_out.outpoint).unwrap();
    assert_eq!(paid.amount, AMOUNT);
    // the payer got change back, and the replayed payload no longer verifies: its input is spent
    let change: u64 = env.chain.unspent_of(&spk_of(PAYER)).iter().map(|(_, u)| u.amount).sum();
    assert_eq!(change + AMOUNT + v.fee, 500_000_000);
    expect_err(
        kob_x402::verify::verify_payment(&ctx, &offer, &payload, &request_hash),
        Reason::InvalidTransactionState,
        Diag::InvalidKaspaExactUtxo,
    );

    // response headers
    let resp = settlement_response(&v, Network::Testnet10);
    assert!(resp.success);
    assert_eq!(resp.transaction, hex(&v.txid));
    assert_eq!(resp.amount.as_deref(), Some("20000000"));
    let rh = kob_x402::client::native::payment_response_header(&resp).unwrap();
    let back: SettlementResponse = header_decode(&rh).unwrap();
    assert_eq!(back, resp);
    assert_eq!(header_encode(&back).unwrap(), rh);
}

#[test]
fn payload_is_deterministic_and_payment_ids_are_fresh() {
    let a = fixture();
    let b = fixture();
    assert_eq!(a.payload.payload.transaction, b.payload.payload.transaction);
    assert_eq!(a.payload.payload.authorization, b.payload.payload.authorization);
    // the payment identifier is drawn at random for every build (a retry re-sends the stored payload)
    let id = |p: &PaymentPayload| p.extensions.as_ref().unwrap()["payment-identifier"]["info"]["id"].as_str().unwrap().to_string();
    assert_ne!(id(&a.payload), id(&b.payload));
}

#[test]
fn payer_can_pay_without_change_and_with_many_inputs() {
    // exact funds: the change would be below the minimum change (0.01 KAS): no change output, the
    // remainder is the fee (the relay floor of this shape is 203600)
    let fx = fixture_with(&[AMOUNT + 900_000], 100);
    let v = fx.verify().expect("no-change payment verifies");
    assert_eq!(v.tx.outputs.len(), 1);
    assert_eq!(v.fee, 900_000);
    // several coins
    let fx = fixture_with(&[9_000_000, 9_000_000, 9_000_000, 9_000_000], 100);
    let v = fx.verify().expect("multi-input payment verifies");
    assert!(v.tx.inputs.len() >= 3);
    assert_eq!(v.consumed.len(), v.tx.inputs.len());
}

#[test]
fn second_funding_key_can_authorize_and_change_may_go_to_it() {
    // two payer inputs of two different keys; the authorization is signed by the second input's key and
    // the change returns to the script of the second input: both are "a verified payer input".
    let fx = fixture_with(&[300_000_000], 100);
    let other = fund(&fx.env.chain, ATTACKER, 300_000_000);
    let (p, _tx, _) = {
        let other = other.clone();
        let r = Repack { resign: false, reauthorize: false, ..Repack::default() };
        mutate(&fx, &r, move |tx, entries| {
            tx.inputs.push(TransactionInput::new(other.utxo.outpoint(), vec![], u64::MAX, 1));
            entries.push(kaspa_consensus_core::tx::UtxoEntry::new(300_000_000, spk_of(ATTACKER), 0, false, None));
            tx.outputs[1].script_public_key = spk_of(ATTACKER);
            // the second input raises the relay floor: part of it goes to the fee
            tx.outputs[1].value += 300_000_000 - 200_000;
        })
    };
    // sign each input with its own key, then authorize with input 1 / the second key
    let (mut tx, entries) = decode(&p);
    for (i, k) in [PAYER, ATTACKER].iter().enumerate() {
        let d = kob_protocol::tx::sighash(&tx, &entries, i);
        tx.inputs[i].signature_script = kob_protocol::script::push_data(&kob_protocol::tx::sign_digest(&secret(*k), &d).unwrap());
    }
    tx.set_storage_mass(kob_protocol::tx::masses(&tx, &entries).storage);
    let mut p = p;
    p.payload.transaction = SafeTx::from_consensus(&tx, &entries).to_text();
    authorize(&mut p, &fx.offer, &fx.request_hash, &tx, &Repack { auth_key: ATTACKER, auth_input: 1, ..Repack::default() });
    let v = fx.verify_payload(&p).expect("two-key payment verifies");
    assert_eq!(v.payer_address, kob_x402::common::address_of(&spk_of(ATTACKER), Network::Testnet10));
    // authorizing with the first input's key but naming the second input fails
    authorize(&mut p, &fx.offer, &fx.request_hash, &tx, &Repack { auth_key: PAYER, auth_input: 1, ..Repack::default() });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
}

// ------------------------------------------------------------------------------------------- revocation

#[test]
fn revocation_makes_the_disclosed_payment_unconfirmable() {
    let fx = fixture_with(&[300_000_000, 200_000_000], 100);
    let v = fx.verify().expect("payment verifies");
    let rev = revoke_native(&fx.payload, &secret(PAYER), 100, 50_000_000).expect("revocation");
    // it spends one input of the payment back to the payer
    assert!(v.consumed.contains(&rev.spent));
    assert_eq!(rev.tx.outputs.len(), 1);
    assert_eq!(rev.tx.outputs[0].script_public_key, spk_of(PAYER));
    kob_protocol::verify::validate(&rev.tx, &rev.entries).expect("revocation is consensus-valid");
    // the revocation confirms; the payment can no longer be broadcast or verified
    fx.env.chain.submit(&rev.tx).expect("revocation accepted into the mempool");
    fx.env.chain.mine(0);
    assert!(matches!(fx.env.chain.submit(&v.tx), Err(SubmitError::Conflict(_))));
    expect_err(fx.verify(), Reason::InvalidTransactionState, Diag::InvalidKaspaExactUtxo);
    // foreign secret or absurd fee cap are refused
    assert!(revoke_native(&fx.payload, &secret(ATTACKER), 100, 50_000_000).is_err());
    assert!(revoke_native(&fx.payload, &secret(PAYER), 100, 1).is_err());
}

// ------------------------------------------------------------------------------------------ attacks

#[test]
fn underpayment_is_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| {
        tx.outputs[0].value -= 1;
        tx.outputs[1].value += 1;
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::Underpayment);
}

#[test]
fn overpayment_is_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| {
        tx.outputs[0].value += 1;
        tx.outputs[1].value -= 1;
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::Overpayment);
}

#[test]
fn wrong_recipient_is_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[0].script_public_key = spk_of(ATTACKER));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn two_merchant_outputs_are_rejected() {
    let fx = fixture();
    // change paid to the merchant as well
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].script_public_key = spk_of(MERCHANT));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
    // a third output that also pays the merchant
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| {
        tx.outputs[1].value -= 1_000_000;
        tx.outputs.push(TransactionOutput::new(1_000_000, spk_of(MERCHANT)));
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
}

#[test]
fn extra_output_to_an_attacker_is_rejected() {
    let fx = fixture();
    // change redirected to the attacker
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].script_public_key = spk_of(ATTACKER));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
    // a third output
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| {
        tx.outputs[1].value -= 1_000_000;
        tx.outputs.push(TransactionOutput::new(1_000_000, spk_of(ATTACKER)));
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // a zero-value change output
    let (p, _, _) = mutate(&fx, &Repack { fix_mass: false, ..Repack::default() }, |tx, _| tx.outputs[1].value = 0);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn wrong_payment_output_index_is_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack { payment_output_index: 1, ..Repack::default() }, |_, _| {});
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
    // and an index that does not exist
    let (p, _, _) = mutate(&fx, &Repack { payment_output_index: 9, ..Repack::default() }, |_, _| {});
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactPaymentOutput);
}

#[test]
fn tampered_accepted_requirements_are_rejected() {
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.accepted.amount = "1".into();
    expect_err(fx.verify_payload(&p), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Accepted);
    let mut p = fx.payload.clone();
    p.accepted.pay_to = addr(ATTACKER);
    expect_err(fx.verify_payload(&p), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Accepted);
    let mut p = fx.payload.clone();
    p.accepted.extra.insert("finality".into(), json!("confirmed"));
    expect_err(fx.verify_payload(&p), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Accepted);
    let mut p = fx.payload.clone();
    p.accepted.extra.insert("smuggled".into(), json!(true));
    expect_err(fx.verify_payload(&p), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Accepted);
}

#[test]
fn tampered_request_hash_is_rejected() {
    let fx = fixture();
    // the payload claims another hash than the resource server computed
    let mut p = fx.payload.clone();
    p.payload.request_hash = hex(&[0x55; 32]);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaX402Payload);
    // the resource server asks for another request than the one authorized
    let other = hex(&[0x66; 32]);
    let r = kob_x402::verify::verify_payment(&fx.env.ctx(), &fx.offer, &fx.payload, &other);
    expect_err(r, Reason::InvalidPayload, Diag::InvalidKaspaX402Payload);
    // consistent-looking payload with the other hash but a stale authorization
    let mut p = fx.payload.clone();
    p.payload.request_hash = other.clone();
    let r = kob_x402::verify::verify_payment(&fx.env.ctx(), &fx.offer, &p, &other);
    expect_err(r, Reason::InvalidPayload, Diag::InvalidAuthorization);
    // no hash at all
    let r = kob_x402::verify::verify_payment(&fx.env.ctx(), &fx.offer, &fx.payload, "");
    expect_err(r, Reason::InvalidPayload, Diag::InvalidKaspaX402Payload);
}

#[test]
fn forged_utxo_hint_is_rejected() {
    let fx = fixture();
    // hint amount lies (the chain says 500_000_000)
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].utxo.as_mut().unwrap().amount = "900000000".into();
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactUtxo);
    // hint script lies on the authorizing input: the authorization is checked against the hinted key before any chain
    // lookup, and that key did not sign it
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].utxo.as_mut().unwrap().script_public_key = kob_x402::safe_tx::spk_to_hex(&spk_of(ATTACKER));
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
    // covenant id hint on a plain coin
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].utxo.as_mut().unwrap().covenant_id = Some(hex(&[1; 32]));
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactUtxo);
    // missing hint: nothing to locate the coin with
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].utxo = None;
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactUtxo);
}

#[test]
fn stated_transaction_id_must_match() {
    let fx = fixture();
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.id = Some(hex(&[0x11; 32]));
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransactionId);
    // no stated id is fine: the verifier recomputes it
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.id = None;
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    fx.verify_payload(&p).expect("id is optional");
}

#[test]
fn spent_input_is_rejected() {
    let fx = fixture();
    fx.env.chain.spend_externally(&outpoint_of_coin(&fx.coins[0]));
    expect_err(fx.verify(), Reason::InvalidTransactionState, Diag::InvalidKaspaExactUtxo);
}

#[test]
fn unavailable_node_fails_closed_and_retryable() {
    let fx = fixture();
    fx.env.chain.set_unavailable(true);
    let e = fx.verify().unwrap_err();
    assert_eq!((e.reason, e.diag, e.retryable), (Reason::UnexpectedSettleError, Diag::NodeUnavailable, true));
}

#[test]
fn expired_and_too_far_authorizations_are_rejected() {
    let fx = fixture();
    // the clock passes the expiry
    fx.env.clock.set(NOW_MS + TIMEOUT * 1000);
    expect_err(fx.verify(), Reason::InvalidTransactionState, Diag::ExpiredAuthorization);
    fx.env.clock.set(NOW_MS + TIMEOUT * 1000 - 1);
    fx.verify().expect("one millisecond before expiry");
    fx.env.clock.set(NOW_MS);
    // expiry beyond now + maxTimeoutSeconds (correctly signed)
    let (p, _, _) = mutate(&fx, &Repack { expires_ms: NOW_MS + TIMEOUT * 1000 + 1, resign: false, ..Repack::default() }, |_, _| {});
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::AuthorizationExceedsMaxTimeout);
    // already expired at signing time
    let (p, _, _) = mutate(&fx, &Repack { expires_ms: NOW_MS, resign: false, ..Repack::default() }, |_, _| {});
    expect_err(fx.verify_payload(&p), Reason::InvalidTransactionState, Diag::ExpiredAuthorization);
    // malformed timestamp
    let mut p = fx.payload.clone();
    p.payload.authorization.expires_at = "2099-01-01".into();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
}

#[test]
fn authorization_signed_by_the_wrong_key_is_rejected() {
    let fx = fixture();
    // digest correct, signature by another key
    let (p, _, _) = mutate(&fx, &Repack { auth_key: ATTACKER, resign: false, ..Repack::default() }, |_, _| {});
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
    // truncated / garbage signature
    let mut p = fx.payload.clone();
    p.payload.authorization.signature = Some("ab".repeat(63));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    let mut p = fx.payload.clone();
    p.payload.authorization.signature = None;
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    let mut p = fx.payload.clone();
    p.payload.authorization.signature = Some("00".repeat(64));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
}

#[test]
fn authorization_digest_mismatch_and_bad_shape_are_rejected() {
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.payload.authorization.digest = hex(&[0x42; 32]);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    let mut p = fx.payload.clone();
    p.payload.authorization.digest = "zz".into();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    // input index missing / out of range
    let mut p = fx.payload.clone();
    p.payload.authorization.input_index = None;
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    let mut p = fx.payload.clone();
    p.payload.authorization.input_index = Some(5);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    // wrong authorization scheme (the KOB payload commitment does not apply to standard-native)
    let mut p = fx.payload.clone();
    p.payload.authorization.version = kob_x402::wire::AUTH_VERSION_PAYLOAD.into();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
    // an authorization for another transaction (replayed onto a different tx) fails the digest
    let other = fixture_with(&[400_000_000], 100);
    let mut p = fx.payload.clone();
    p.payload.authorization = other.payload.payload.authorization.clone();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidAuthorization);
}

#[test]
fn oversized_artifacts_are_rejected() {
    let mut fx = fixture();
    fx.env.policy.limits.max_tx_json_bytes = 200;
    expect_err(fx.verify(), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    let mut fx = fixture();
    fx.env.policy.limits.max_inputs = 0;
    expect_err(fx.verify(), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    let mut fx = fixture();
    fx.env.policy.limits.max_outputs = 1;
    expect_err(fx.verify(), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    let mut fx = fixture();
    fx.env.policy.limits.max_signature_script_bytes = 10;
    expect_err(fx.verify(), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // a huge signature script inside an otherwise valid-looking artifact
    let fx = fixture();
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.inputs[0].signature_script = "00".repeat(40_000);
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
}

#[test]
fn duplicate_inputs_are_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, entries| {
        let dup = tx.inputs[0].clone();
        tx.inputs.push(dup);
        entries.push(entries[0].clone());
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
}

#[test]
fn non_empty_payload_gas_lock_time_and_version_are_rejected() {
    // rebuilt at a fee margin so the (consensus-accepted) mutations do not fall below the fee floor first
    let fx = fixture_with(&[500_000_000], 150);
    fx.verify().expect("control");
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| tx.payload = vec![1]);
    kob_protocol::verify::validate(&tx, &entries).expect("engine accepts a payload");
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    let (p, tx, entries) = mutate(&fx, &Repack::default(), |tx, _| tx.gas = 1);
    kob_protocol::verify::validate(&tx, &entries).expect("the script engine does not see the gas rule; the profile does");
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.lock_time = 1);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // version 1 is not part of standard-native (the artifact cannot even carry a version-0 sigop count)
    let (p, _, _) =
        mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, _| tx.version = 1);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // covenant binding on an output (version 1 only, so the profile rejects on version first)
    let mut safe = SafeTx::parse(&fx.payload.payload.transaction, 1 << 20).unwrap();
    safe.outputs[1].covenant = Some(kob_x402::safe_tx::SafeCovenant { authorizing_input: 0, covenant_id: hex(&[2; 32]) });
    let mut p = fx.payload.clone();
    p.payload.transaction = safe.to_text();
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
}

#[test]
fn wrong_storage_mass_commitment_is_rejected() {
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, _| {
        tx.set_storage_mass(tx.storage_mass() + 1)
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactMass);
    let (p, _, _) = mutate(&fx, &Repack { fix_mass: false, resign: false, reauthorize: false, ..Repack::default() }, |tx, _| {
        tx.set_storage_mass(0)
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactMass);
}

#[test]
fn fee_above_policy_and_below_floor_are_rejected() {
    let mut fx = fixture();
    let v = fx.verify().expect("control");
    fx.env.policy.limits.max_fee_sompi = v.fee - 1;
    expect_err(fx.verify(), Reason::InvalidPayload, Diag::InvalidKaspaExactFee);
    fx.env.policy.limits.max_fee_sompi = v.fee;
    fx.verify().expect("fee equal to the bound is allowed");
    // below the relay floor (fee shaved off by growing the change; storage mass re-committed)
    let fx = fixture();
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].value += 1_000);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactFee);
    // outputs exceeding inputs
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| tx.outputs[1].value += 1_000_000_000);
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactFee);
}

#[test]
fn missing_or_malformed_payment_identifier_is_rejected() {
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.extensions = None;
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::MissingKaspaPaymentIdentifier);
    let mut p = fx.payload.clone();
    p.extensions = Some(json!({ "payment-identifier": { "info": { "required": true } } }));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::MissingKaspaPaymentIdentifier);
    let mut p = fx.payload.clone();
    p.extensions = Some(json!({ "payment-identifier": { "info": { "required": true, "id": "short" } } }));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaPaymentIdentifier);
    let mut p = fx.payload.clone();
    p.extensions = Some(json!({ "payment-identifier": { "info": { "required": true, "id": "has spaces in it 0123456789" } } }));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaPaymentIdentifier);
    // a deployment that does not require the extension accepts its absence
    let mut fx2 = fixture();
    fx2.env.policy.require_payment_identifier = false;
    let mut p = fx2.payload.clone();
    p.extensions = None;
    assert_eq!(fx2.verify_payload(&p).unwrap().payment_identifier, None);
}

#[test]
fn profile_shape_rules_on_inputs_and_offer() {
    let fx = fixture();
    // a P2SH-locked input is not a standard P2PK funding input
    let p2sh = kob_protocol::script::p2sh_spk(&[0x51]);
    let op = fx.env.chain.add_utxo(50_000_000, p2sh.clone(), None);
    let (p, _, _) = mutate(&fx, &Repack { resign: false, reauthorize: false, ..Repack::default() }, |tx, entries| {
        tx.inputs.push(TransactionInput::new(
            kaspa_consensus_core::tx::TransactionOutpoint::new(kaspa_consensus_core::tx::TransactionId::from_bytes(op.txid), op.index),
            vec![0x51],
            u64::MAX,
            0,
        ));
        entries.push(kaspa_consensus_core::tx::UtxoEntry::new(50_000_000, p2sh, 0, false, None));
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // sigop commitment other than one
    let (p, _, _) = mutate(&fx, &Repack::default(), |tx, _| {
        let i = tx.inputs[0].clone();
        tx.inputs[0] = TransactionInput::new(i.previous_outpoint, i.signature_script, i.sequence, 2);
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactTransaction);
    // non-SIGHASH_ALL signature (SIGHASH_NONE would let anyone edit the outputs)
    let (p, _, _) = mutate(&fx, &Repack { reauthorize: false, resign: false, ..Repack::default() }, |tx, _| {
        let n = tx.inputs[0].signature_script.len();
        tx.inputs[0].signature_script[n - 1] = 0x02;
    });
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
    // additive fields in a standard-native offer, additive challenge in the payload
    let mut fx2 = fixture();
    fx2.offer.extra.insert("challengeId".into(), json!(hex(&[1; 32])));
    fx2.payload.accepted = fx2.offer.clone();
    expect_err(fx2.verify(), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Binding);
    let mut p = fx.payload.clone();
    p.payload.challenge_id = Some(hex(&[1; 32]));
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaX402Payload);
    // amount below the policy minimum
    let mut fx3 = fixture();
    fx3.env.policy.limits.min_amount_sompi = AMOUNT + 1;
    expect_err(fx3.verify(), Reason::InvalidPaymentRequirements, Diag::InvalidKaspaX402Amount);
}

#[test]
fn additive_and_unknown_profiles_are_unsupported() {
    let fx = fixture();
    let mut p = fx.payload.clone();
    p.payload.profile = "additive".into();
    expect_err(fx.verify_payload(&p), Reason::UnsupportedScheme, Diag::UnsupportedKaspaExactProfile);
    let mut p = fx.payload.clone();
    p.payload.profile = "streaming".into();
    expect_err(fx.verify_payload(&p), Reason::UnsupportedScheme, Diag::UnsupportedKaspaExactProfile);
}

#[test]
fn only_the_verifier_network_is_served() {
    let fx = fixture();
    let mut env = Env::new();
    env.policy = kob_x402::policy::Policy::new(Network::Mainnet);
    let r = kob_x402::verify::verify_payment(&env.ctx(), &fx.offer, &fx.payload, &fx.request_hash);
    expect_err(r, Reason::InvalidNetwork, Diag::InvalidKaspaX402Binding);
}

#[test]
fn signature_of_another_key_inside_the_transaction_fails_the_engine() {
    let fx = fixture();
    let (p, tx, entries) = mutate(&fx, &Repack { resign: false, ..Repack::default() }, |tx, entries| {
        // sign input 0 with a key that does not own the coin (canonical shape, wrong key)
        let d = kob_protocol::tx::sighash(tx, entries, 0);
        tx.inputs[0].signature_script =
            kob_protocol::script::push_data(&kob_protocol::tx::sign_digest(&secret(ATTACKER), &d).unwrap());
    });
    assert!(kob_protocol::verify::validate(&tx, &entries).is_err());
    expect_err(fx.verify_payload(&p), Reason::InvalidPayload, Diag::InvalidKaspaExactSignature);
    let _ = pubkey(PAYER);
}

// ------------------------------------------------------------------------------------------ SIGHASH_ALL

#[test]
fn every_payer_signature_must_use_sighash_all() {
    use kob_x402::testkit::hashtype::{resigned, signers, tampered_byte, BOGUS, NON_ALL};
    // one funding input, then two (every signature is checked, not only the first)
    for funds in [vec![500_000_000u64], vec![15_000_000, 15_000_000]] {
        let fx = fixture_with(&funds, 100);
        let (tx, entries) = fx.tx();
        let keys: std::collections::BTreeMap<_, _> = [(pubkey(PAYER), secret(PAYER))].into();
        let ss = signers(&tx, &entries, &keys);
        assert_eq!(ss.len(), funds.len());
        fx.verify().unwrap();
        for s in &ss {
            // control: SIGHASH_ALL again verifies; every other valid hash type is accepted by the engine and refused here
            fx.verify_payload(&mutate_tx(&fx, &resigned(&tx, &entries, s, 0x01), &entries)).unwrap();
            for t in NON_ALL {
                let bad = resigned(&tx, &entries, s, t);
                kob_protocol::verify::validate(&bad, &entries).unwrap_or_else(|e| panic!("the engine must accept {t:#04x}: {e}"));
                expect_err(
                    fx.verify_payload(&mutate_tx(&fx, &bad, &entries)),
                    Reason::InvalidPayload,
                    Diag::InvalidKaspaExactSignature,
                );
            }
            for t in BOGUS {
                expect_err(
                    fx.verify_payload(&mutate_tx(&fx, &tampered_byte(&tx, s, t), &entries)),
                    Reason::InvalidPayload,
                    Diag::InvalidKaspaExactSignature,
                );
            }
        }
    }
}

/// The fixture's payload with its transaction replaced; the authorization stays valid (the transaction id does not
/// cover signature scripts).
fn mutate_tx(
    fx: &Fixture,
    tx: &kaspa_consensus_core::tx::Transaction,
    entries: &[kaspa_consensus_core::tx::UtxoEntry],
) -> kob_x402::wire::PaymentPayload {
    kob_x402::testkit::hashtype::with_tx(&fx.payload, tx, entries)
}

#[test]
fn the_payers_total_bound_counts_the_fee() {
    let fx = fixture();
    let v = fx.verify().unwrap();
    let total = AMOUNT + v.fee;
    let mut opts = PayOptions::new(u64::MAX);
    opts.max_total_sompi = Some(total - 1);
    let pay = |o: &PayOptions| kob_x402::client::native::pay_native(&fx.offer, &fx.request_hash, &secret(PAYER), &fx.coins, NOW_MS, o);
    let e = pay(&opts).unwrap_err();
    assert!(
        matches!(e, kob_x402::client::native::NativeError::SpendAboveLimit { spend, max } if spend == total && max == total - 1),
        "{e}"
    );
    opts.max_total_sompi = Some(total);
    pay(&opts).unwrap();
}
