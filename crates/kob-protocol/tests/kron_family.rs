//! KRON family specifics of the builders, the sigscript layout and the `KOB1` record. The whole
//! builder set on both real KRON programs is validated in `builders.rs`; this file pins what is
//! different from KCC-20 and what the builders refuse.

mod common;

use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::build::{build, Action, Leg};
use kob_protocol::family::{KRON_TYPE_COVID, KRON_TYPE_PUBKEY};
use kob_protocol::payload::{self, decode, Custody, Record};
use kob_protocol::registry::{Family as RegFamily, Registry};
use kob_protocol::state::{AnyState, KronState, TokenState};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, SigPlan};
use kob_protocol::verify::validate_signed;
use kob_protocol::{Error, Family};

const KRON: TemplateId = TemplateId::KronToken2433;

fn kron(name: &str) -> Action {
    common::scenarios_on(KRON).into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no KRON scenario {name}")).1
}
fn kcc20(name: &str) -> Action {
    common::scenarios().into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no scenario {name}")).1
}
fn refused(a: &Action, what: &str, needle: &str) {
    match build(a) {
        Err(e @ (Error::Invalid(_) | Error::State(_))) if e.to_string().contains(needle) => {}
        other => panic!("{what}: expected a refusal mentioning {needle:?}, got {other:?}"),
    }
}

/// Pushes of a signature script (direct pushes and `OP_PUSHDATA1/2`).
fn pushes(mut s: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    while !s.is_empty() {
        let (n, skip) = match s[0] {
            0x00 => (0, 1),
            n @ 0x01..=0x4b => (n as usize, 1),
            0x4c => (s[1] as usize, 2),
            0x4d => (u16::from_le_bytes([s[1], s[2]]) as usize, 3),
            op => panic!("unexpected opcode {op:#04x} in a KRON token sigscript"),
        };
        out.push(s[skip..skip + n].to_vec());
        s = &s[skip + n..];
    }
    out
}

#[test]
fn key_held_kron_tokens_need_the_owners_p2pk_input() {
    // A taker sells tokens (address presence): without a funding input of the taker there is nothing
    // that authorises the token input.
    let Action::Batch(mut b) = kron("take.bid.partial") else { panic!() };
    assert!(b.funding.iter().any(|f| f.pubkey == common::pk(common::TAKER)));
    b.funding.clear();
    refused(&Action::Batch(b), "presence without a P2PK input", "P2PK input of that key");
    // Maker tokens of a creation come from a key the funding does not include.
    let Action::CreateOrder(mut c) = kron("create.ask") else { panic!() };
    c.funding = vec![common::key_utxo(2, common::MAKER_B, 1_000 * common::KAS)];
    refused(&Action::CreateOrder(c), "creation funded by another key", "P2PK input of that key");
}

#[test]
fn every_token_input_carries_the_same_columns_and_its_own_witness() {
    // Top-up cancel-replace: the custody (id_type 2, authorised by the order input 0) and the maker's
    // top-up tokens (address presence, authorised by the maker's funding input).
    let action = kron("cancelReplace.ask.topUp");
    let built = build(&action).unwrap();
    let mut token_inputs = vec![];
    for (i, p) in built.plans.iter().enumerate() {
        if let SigPlan::KronToken { state, next_states, witnesses, .. } = p {
            token_inputs.push((i, state.clone(), next_states.clone(), witnesses.clone()));
        }
    }
    assert_eq!(token_inputs.len(), 2);
    // The same next-state columns and witness column on every token input.
    assert_eq!(token_inputs[0].2, token_inputs[1].2);
    assert_eq!(token_inputs[0].3, token_inputs[1].3);
    let funding_at = built.plans.iter().position(|p| matches!(p, SigPlan::P2pk { .. })).unwrap() as u8;
    // Witness column: one byte per token input in input order: [order input 0 (custody), funding (maker tokens)].
    assert_eq!(token_inputs[0].1.id_type, KRON_TYPE_COVID);
    assert_eq!(token_inputs[0].3, vec![0, funding_at]);
    // No token input is signed: address presence is the funding input's own signature.
    let signers: Vec<usize> = built.sign.iter().map(|s| s.input_index).collect();
    assert!(token_inputs.iter().all(|(i, ..)| !signers.contains(i)));
    // Token outputs are bound to the first token input (the "leader" of the covenant group).
    let first_token = token_inputs[0].0 as u16;
    for o in &built.tx.outputs {
        if o.covenant.as_ref().is_some_and(|c| c.covenant_id == common::TOKEN_COV) {
            assert_eq!(o.covenant.as_ref().unwrap().authorizing_input, first_token);
        }
    }
    // It is a valid transaction.
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default()).unwrap();
    validate_signed(&signed).unwrap();
}

#[test]
fn kron_sigscript_is_seven_pushes_without_a_dispatch_tag() {
    let built = build(&kron("take.ask.partial")).unwrap();
    let (i, plan) = built.plans.iter().enumerate().find(|(_, p)| matches!(p, SigPlan::KronToken { .. })).unwrap();
    let SigPlan::KronToken { next_states, witnesses, template, state, .. } = plan else { unreachable!() };
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default()).unwrap();
    let ss = kob_protocol::json::from_hex(&{
        // The signature script of the token input, from the signed transaction's serialised form.
        let (tx, _) = signed.tx.to_tx().unwrap();
        kob_protocol::json::to_hex(&tx.inputs[i].signature_script)
    })
    .unwrap();
    let p = pushes(&ss);
    let k = next_states.len();
    assert_eq!(p.len(), 7, "owners | types | amounts | minters | sigs | witnesses | redeem");
    assert_eq!(p[0], next_states.iter().flat_map(|s| s.owner).collect::<Vec<u8>>());
    assert_eq!(p[1], next_states.iter().map(|s| s.id_type).collect::<Vec<u8>>());
    assert_eq!(p[2], next_states.iter().flat_map(|s| s.amount.to_le_bytes()).collect::<Vec<u8>>());
    assert_eq!(p[3], vec![0u8; k]);
    assert_eq!(p[4], Vec::<u8>::new());
    assert_eq!(p[5], *witnesses);
    assert_eq!(p[6], state.redeem_with(token_template(*template)));
    assert_eq!(p[6].len(), 2433);
}

#[test]
fn minters_and_key_types_the_builders_do_not_authorise_are_refused() {
    let Action::Batch(mut b) = kron("take.bid.partial") else { panic!() };
    let TokenState::Kron(k) = &mut b.taker_tokens[0].state else { panic!("KRON fixture") };
    let good = k.clone();
    k.is_minter = 1;
    refused(&Action::Batch(b.clone()), "minter", "minter");
    // Tokens held by a pubkey (id_type 0, token-level signature) are not key-owned in the builders' sense.
    b.taker_tokens[0].state = TokenState::Kron(KronState { is_minter: 0, id_type: KRON_TYPE_PUBKEY, ..good.clone() });
    refused(&Action::Batch(b), "id_type 0 taker tokens", "key-owned");
    let Action::SendTokens(mut s) = kron("send.tokens") else { panic!() };
    s.tokens[0].state = TokenState::Kron(KronState { id_type: KRON_TYPE_PUBKEY, ..good });
    refused(&Action::SendTokens(s), "id_type 0 sent", "id_type 0");
}

#[test]
fn outputs_above_the_kron_token_cap_are_refused() {
    // The KRON program rejects any output above 1e9 units: the builders never produce one.
    let Action::SendTokens(mut s) = kron("send.tokens") else { panic!() };
    for t in &mut s.tokens {
        t.state = t.state.with_amount(1_500_000_000);
    }
    s.recipients[0].amount = 1_200_000_000;
    refused(&Action::SendTokens(s), "output above 1e9", "1..=1000000000");
    // A custody above the cap can never be moved.
    let Action::CreateOrder(mut c) = kron("create.ask") else { panic!() };
    if let AnyState::KobAskKron(a) = &mut c.order {
        a.amount_left = 2_000_000_000;
    }
    refused(&Action::CreateOrder(c), "custody above the cap", "at most 1000000000");
    // A minimum fill above the cap could never fill partially (every fill is one token output).
    let Action::CreateOrder(mut c) = kron("create.bid") else { panic!() };
    if let AnyState::KobBidKron(b) = &mut c.order {
        b.min_fill = 1_000_000_001;
    }
    refused(&Action::CreateOrder(c), "minimum fill above the cap", "at most 1000000000");
}

#[test]
fn family_mismatches_are_refused() {
    // A KCC-20 order kind on a KRON program and vice versa.
    let Action::CreateOrder(mut c) = kron("create.ask") else { panic!() };
    c.order = c.order.clone().into_family(Family::Kcc20);
    refused(&Action::CreateOrder(c), "KCC-20 kind, KRON token", "family");
    let Action::CreateOrder(mut c) = kcc20("create.ask") else { panic!() };
    c.order = c.order.clone().into_family(Family::Kron);
    refused(&Action::CreateOrder(c), "KRON kind, KCC-20 token", "family");
    // A token UTXO of the other family under a token.
    let Action::Batch(mut b) = kron("take.bid.partial") else { panic!() };
    b.taker_tokens[0].state = common::tok(1, common::WHOLE, common::pk(common::TAKER), 0, 900).state;
    refused(&Action::Batch(b), "KCC-20 state on a KRON token", "token state on the");
    // KRON tokens have no extension commitment.
    let Action::CreateOrder(mut c) = kron("create.bid") else { panic!() };
    if let AnyState::KobBidKron(b) = &mut c.order {
        b.extension_commitment = common::EXT;
    }
    refused(&Action::CreateOrder(c), "extension commitment on KRON", "extension commitment");
    // A token program that is neither family's.
    let Action::CreateOrder(mut c) = kron("create.ask") else { panic!() };
    if let AnyState::KobAskKron(a) = &mut c.order {
        a.token_tpl_hash = [7; 32];
    }
    refused(&Action::CreateOrder(c), "unknown program", "not a supported");
}

/// v2.6: trigger evidence is per token (a plain leg of the same token in the same transaction), not per family:
/// a batch may arm a KCC-20 stop and trade a KRON book at once, but a KRON fill never arms a KCC-20 stop.
#[test]
fn touch_evidence_is_per_token_and_batches_may_mix_families() {
    let Action::Batch(mut b) = kcc20("cond.ask.stop.trigger") else { panic!() };
    let Action::Batch(k) = kron("take.ask.partial") else { panic!() };
    let mut leg = k.legs[0].clone();
    if let Leg::Ask { order, custody, .. } = &mut leg {
        order.utxo.covenant_id = Some([0x99; 32]);
        order.utxo.transaction_id = [0x91; 32];
        custody.utxo.transaction_id = [0x92; 32];
        order.state.token_cov_id = common::TOKEN_B;
        custody.utxo.covenant_id = Some(common::TOKEN_B);
        common::set_owner(&mut custody.state, [0x99; 32]);
    }
    b.legs.push(leg);
    let built = build(&Action::Batch(b.clone())).expect("a KCC-20 stop armed next to a KRON fill");
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default()).unwrap();
    validate_signed(&signed).unwrap();
    // The KRON ask (another token and family) as the KCC-20 stop's evidence is refused.
    if let Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = Some(2);
    }
    refused(&Action::Batch(b), "a KRON fill as a KCC-20 stop's evidence", "another token");
}

#[test]
fn placement_record_of_a_kron_order() {
    let action = kron("create.ask");
    let built = build(&action).unwrap();
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default()).unwrap();
    let p = decode(&signed.tx.payload).unwrap().unwrap();
    assert_eq!(p.version, payload::PAYLOAD_VERSION);
    let Record::Order { family, template, state, custody, .. } = &p.records[0] else { panic!() };
    assert_eq!((*family, *template), (payload::FAMILY_KRON46, TemplateId::KobAskKron));
    // KRON custody part: the token output only (no extension commitment).
    assert_eq!(custody, &Some(Custody { token_output: 1, extension_commitment: [0; 32] }));
    // Version 3: no template hash and no push opcodes; the version-2 record was output 2 + family 1 + kind 1 + template
    // hash 32 + state length 2 + state + custody 2.
    let v2_len = 4 + 1 + 3 + 2 + 1 + 1 + 32 + 2 + state.len() + 2;
    assert!(signed.tx.payload.len() + 32 + 19 < v2_len, "{} vs {v2_len}", signed.tx.payload.len());
    // The KCC-20 record of the same order also spells its custody's extension commitment.
    let kcc = build(&kcc20("create.ask")).unwrap();
    assert!(kcc.tx.payload.len() >= signed.tx.payload.len() + 32);
    // Recovery re-derives the KRON order and its type-2 custody output.
    let rec = payload::recover_orders(&signed.tx).unwrap();
    assert_eq!(rec.len(), 1);
    let c = rec[0].custody.as_ref().unwrap();
    assert!(matches!(&c.state, TokenState::Kron(k) if k.id_type == KRON_TYPE_COVID && k.owner == rec[0].covenant_id));
    // The KCC-20 family byte names the KCC-20 template of the kind: the state no longer matches the output.
    let fam_at = 4 + 1 + 3 + 1;
    let mut forged = signed.clone();
    forged.tx.payload[fam_at] = payload::FAMILY_KCC20;
    assert!(
        decode(&forged.tx.payload).is_err() || payload::recover_orders(&forged.tx).is_err(),
        "family byte and template must agree"
    );
    let mut unknown = signed.tx.payload.clone();
    unknown[fam_at] = 0x03;
    assert!(decode(&unknown).is_err());
}

/// v2.6 retired the receipt: its genesis record (type 0x02) and its order kind (0x07) never decode again, in
/// either family and either payload version (the numbers stay reserved).
#[test]
fn retired_receipt_records_do_not_decode() {
    for version in [payload::PAYLOAD_VERSION_2, payload::PAYLOAD_VERSION] {
        for fam in [payload::FAMILY_KCC20, payload::FAMILY_KRON46] {
            // RECEIPT_GENESIS: count 16 (+ family byte for KRON)
            let mut v = b"KOB1".to_vec();
            v.push(version);
            let body: Vec<u8> = if fam == payload::FAMILY_KCC20 { vec![16, 0] } else { vec![16, 0, fam] };
            v.push(payload::REC_RETIRED_RECEIPT_GENESIS);
            v.extend_from_slice(&(body.len() as u16).to_le_bytes());
            v.extend_from_slice(&body);
            assert!(decode(&v).unwrap_err().to_string().contains("retired"), "record 0x02, family {fam}, version {version}");
            // An order record of kind 0x07 (version 2: output u16, family, kind, template hash...; version 3: output
            // LEB128, family, kind, flags, state...).
            let mut rec = if version == payload::PAYLOAD_VERSION_2 { vec![0u8, 0, fam, 0x07] } else { vec![0u8, fam, 0x07, 0] };
            rec.extend_from_slice(&[0x55; 32]);
            rec.extend_from_slice(&120u16.to_le_bytes());
            rec.extend_from_slice(&[0; 120]);
            let mut v = b"KOB1".to_vec();
            v.push(version);
            v.push(payload::REC_ORDER);
            v.extend_from_slice(&(rec.len() as u16).to_le_bytes());
            v.extend_from_slice(&rec);
            assert!(decode(&v).unwrap_err().to_string().contains("retired"), "kind 0x07, family {fam}, version {version}");
        }
    }
    assert!(TemplateId::ALL.iter().all(|t| t.kind_code() != Some(0x07)));
}

#[test]
fn the_registry_pins_the_same_token_programs() {
    let reg = Registry::default_registry();
    let mut kron_seen = 0;
    for t in &reg.templates {
        let h = kob_protocol::registry::parse_hex32(&t.template_hash).unwrap();
        let tt = token_template_by_hash(&h).unwrap_or_else(|| panic!("registry template {} is not embedded", t.id));
        assert_eq!(
            (tt.family, tt.prefix.len() as u32, tt.suffix.len() as u32, tt.state_len as u32, tt.slots.0 as u32, tt.slots.1 as u32),
            (
                match t.family {
                    RegFamily::Kcc20 => Family::Kcc20,
                    RegFamily::Kron => Family::Kron,
                },
                t.prefix_len,
                t.suffix_len,
                t.state_len,
                t.max_token_inputs,
                t.max_token_outputs
            ),
            "{}",
            t.id
        );
        if t.family == RegFamily::Kron {
            kron_seen += 1;
            assert_eq!(t.escrow.id_type, Some(KRON_TYPE_COVID));
            assert_eq!(t.escrow.delivery_id_type, Some(kob_protocol::family::KRON_TYPE_ADDR));
        }
    }
    assert_eq!(kron_seen, 2, "both real KRON programs are in the registry");
}

/// A route may cross families: payer's KCC-20 tokens are sold into KCC-20 bids and the KAS buys a KRON
/// token from KRON asks in the same transaction.
#[test]
fn a_route_can_cross_families() {
    let Action::SwapRoute(mut r) = kcc20("route.swap") else { panic!() };
    let Action::SwapRoute(k) = kron("route.swap") else { panic!() };
    r.buy = k.buy;
    let built = build(&Action::SwapRoute(r)).unwrap();
    let signed = finalize(&built, &sign_locally(&built, &common::keys()).unwrap(), FinalizeOptions::default()).unwrap();
    validate_signed(&signed).unwrap();
    // Both token kinds are present: KCC-20 leader / delegators and a KRON token input.
    assert!(built.plans.iter().any(|p| matches!(p, SigPlan::TokenLeader { .. })));
    assert!(built.plans.iter().any(|p| matches!(p, SigPlan::KronToken { .. })));
}
