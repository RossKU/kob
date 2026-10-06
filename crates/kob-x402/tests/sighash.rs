//! The SIGHASH_ALL rule of payer signatures, parsed per owner scheme (`kob_x402::sighash`): every KCC-20 owner
//! scheme (0x00 Schnorr P2PK, 0x01 hashed Schnorr key, 0x02 hashed ECDSA key, 0x03 P2SH authority, 0x04 covenant id)
//! as leader and as delegator, on every token program, and the Schnorr P2PK funding input. The verifier-level
//! cases (a correctly signed non-ALL transaction that the engine accepts) are in `kcc20.rs`, `swap.rs`, `intent.rs`
//! and `exact_e2e.rs`; this file pins the parser itself.

use kob_protocol::artifacts::TemplateId;
use kob_protocol::script::push_data;
use kob_protocol::state::Kcc20State;
use kob_protocol::tx::{SigPlan, Witness};
use kob_x402::error::Diag;
use kob_x402::sighash::{
    check_p2pk_input, check_token_input, hash_type_name, owner_proof, p2pk_hash_type, token_owner_proof, token_witness, OwnerProof,
    TokenWitness, OWNER_COVENANT_ID, OWNER_P2PKH_ECDSA, OWNER_P2PKH_SCHNORR, OWNER_P2PK_SCHNORR, OWNER_P2SH, SIGHASH_ALL,
};

use kob_x402::testkit::hashtype::{BOGUS, NON_ALL};

const EXT: [u8; 32] = [0xee; 32];

const PROGRAMS: [TemplateId; 6] = [
    TemplateId::Kcc20Ref,
    TemplateId::Kcc20Ref4x5,
    TemplateId::Kcc20Ref8x8,
    TemplateId::Kcc20Ref16x16,
    TemplateId::Kcc20P2,
    TemplateId::Kcc20KaspaCom025,
];

/// The owner proof of each scheme, ending in hash type `t` (schemes 0x03 / 0x04 carry none).
fn proof(scheme: u8, t: u8) -> Vec<u8> {
    let sig = |t: u8| [[0x5a; 64].as_slice(), &[t]].concat();
    match scheme {
        OWNER_P2PK_SCHNORR => sig(t),
        OWNER_P2PKH_SCHNORR => [[0x11; 32].as_slice(), &sig(t)].concat(),
        OWNER_P2PKH_ECDSA => [[0x22; 33].as_slice(), &sig(t)].concat(),
        OWNER_P2SH => vec![3],
        OWNER_COVENANT_ID => vec![],
        _ => unreachable!(),
    }
}

fn leader(program: TemplateId, witness: Option<&[u8]>) -> Vec<u8> {
    let plan = SigPlan::TokenLeader {
        template: program,
        state: Kcc20State::p2pk(10, [1; 32], EXT),
        next_states: vec![Kcc20State::p2pk(10, [2; 32], EXT)],
        witness: if witness.is_some() { Witness::P2pk([1; 32]) } else { Witness::CovenantId },
    };
    plan.sigscript(witness).unwrap()
}

fn delegator(program: TemplateId, witness: Option<&[u8]>) -> Vec<u8> {
    let plan = SigPlan::TokenDelegator {
        template: program,
        state: Kcc20State::p2pk(10, [1; 32], EXT),
        witness: if witness.is_some() { Witness::P2pk([1; 32]) } else { Witness::CovenantId },
    };
    plan.sigscript(witness).unwrap()
}

#[test]
fn owner_proofs_parse_per_scheme_with_the_lengths_the_program_enforces() {
    for t in [SIGHASH_ALL].into_iter().chain(NON_ALL).chain(BOGUS) {
        for scheme in [OWNER_P2PK_SCHNORR, OWNER_P2PKH_SCHNORR, OWNER_P2PKH_ECDSA] {
            assert_eq!(owner_proof(scheme, &proof(scheme, t)), Some(OwnerProof::Signature { hash_type: t }), "scheme {scheme}");
        }
    }
    assert_eq!(owner_proof(OWNER_P2SH, &[3]), Some(OwnerProof::Authority { input: 3 }));
    assert_eq!(owner_proof(OWNER_COVENANT_ID, &[]), Some(OwnerProof::Covenant));
    // a proof of another scheme's length, empty, oversize, and an unknown scheme are malformed
    for (scheme, len) in [(0, 64), (0, 66), (0, 97), (1, 65), (1, 98), (2, 97), (2, 99), (3, 0), (3, 2), (4, 1), (5, 65), (0xff, 0)] {
        assert_eq!(owner_proof(scheme, &vec![0x01; len]), None, "scheme {scheme} length {len}");
    }
}

#[test]
fn hash_type_names() {
    assert_eq!(hash_type_name(0x01), "SIGHASH_ALL");
    assert_eq!(hash_type_name(0x02), "SIGHASH_NONE");
    assert_eq!(hash_type_name(0x04), "SIGHASH_SINGLE");
    assert_eq!(hash_type_name(0x81), "SIGHASH_ALL|ANYONECANPAY");
    assert_eq!(hash_type_name(0x82), "SIGHASH_NONE|ANYONECANPAY");
    assert_eq!(hash_type_name(0x84), "SIGHASH_SINGLE|ANYONECANPAY");
    assert!(
        hash_type_name(0x00).contains("unknown")
            && hash_type_name(0x03).contains("unknown")
            && hash_type_name(0x80).contains("unknown")
    );
}

#[test]
fn schnorr_p2pk_funding_input() {
    let sig = |t: u8| [[0x5a; 64].as_slice(), &[t]].concat();
    check_p2pk_input(0, &push_data(&sig(SIGHASH_ALL))).unwrap();
    for t in NON_ALL.into_iter().chain(BOGUS) {
        let e = check_p2pk_input(4, &push_data(&sig(t))).unwrap_err();
        assert_eq!(e.diag, Diag::TokenOwnerScheme, "{t:#04x}: {e}");
        assert!(e.message.contains("input 4"), "{e}");
    }
    assert_eq!(p2pk_hash_type(&push_data(&sig(0x82))), Some(0x82));
    // an OP_PUSHDATA1 spelling of the same 65 bytes reads the same hash type
    let mut pd1 = vec![0x4c, 65];
    pd1.extend(sig(0x84));
    assert_eq!(p2pk_hash_type(&pd1), Some(0x84));
    assert_eq!(check_p2pk_input(0, &pd1).unwrap_err().diag, Diag::TokenOwnerScheme);
    // anything but exactly one 65-byte push is a malformed signature script, not a verdict on the hash type
    for bad in [vec![], push_data(&[0u8; 64]), push_data(&[0u8; 66]), [push_data(&sig(1)), push_data(&[1])].concat(), vec![0xac]] {
        assert_eq!(p2pk_hash_type(&bad), None);
        assert_eq!(check_p2pk_input(0, &bad).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
    }
}

#[test]
fn token_inputs_leader_and_delegator_on_every_program_and_scheme() {
    for program in PROGRAMS {
        // the witness is found at the same place for every program: leader (arrays, witness, tag, redeem) and
        // delegator (witness, tag, redeem)
        for scheme in [OWNER_P2PK_SCHNORR, OWNER_P2PKH_SCHNORR, OWNER_P2PKH_ECDSA] {
            let ok = proof(scheme, SIGHASH_ALL);
            assert_eq!(token_owner_proof(&leader(program, Some(&ok)), true), Some(ok.as_slice()));
            assert_eq!(token_owner_proof(&delegator(program, Some(&ok)), false), Some(ok.as_slice()));
            for t in [SIGHASH_ALL].into_iter().chain(NON_ALL).chain(BOGUS) {
                let p = proof(scheme, t);
                let lead = leader(program, Some(&p));
                let deleg = delegator(program, Some(&p));
                for (script, is_leader) in [(lead, true), (deleg, false)] {
                    let r = check_token_input(2, &script, scheme, is_leader);
                    if t == SIGHASH_ALL {
                        r.unwrap_or_else(|e| panic!("{program:?} scheme {scheme}: {e}"));
                    } else {
                        let e = r.unwrap_err();
                        assert_eq!(
                            e.diag,
                            Diag::TokenOwnerScheme,
                            "{program:?} scheme {scheme} type {t:#04x} leader {is_leader}: {e}"
                        );
                        assert!(e.message.contains("input 2"), "{e}");
                    }
                }
            }
        }
    }
}

#[test]
fn authority_and_covenant_owner_schemes() {
    for program in PROGRAMS {
        // 0x03: the signature is in another input whose script decides: refused
        let p = proof(OWNER_P2SH, 0);
        let e = check_token_input(1, &leader(program, Some(&p)), OWNER_P2SH, true).unwrap_err();
        assert_eq!(e.diag, Diag::TokenOwnerScheme, "{e}");
        assert!(e.message.contains("authorized by input 3"), "{e}");
        // a delegator's one-byte witness is the opcode OP_3 on the wire: still the authority scheme
        let d = delegator(program, Some(&p));
        assert_eq!(token_witness(&d, false), Some(TokenWitness::SmallNumber));
        let e = check_token_input(1, &d, OWNER_P2SH, false).unwrap_err();
        assert_eq!(e.diag, Diag::TokenOwnerScheme, "{e}");
        assert!(e.message.contains("owner scheme 0x03"), "{e}");
        // ... and not a signature scheme's witness
        assert_eq!(check_token_input(1, &d, OWNER_P2PK_SCHNORR, false).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
        // 0x04: no signature at all, nothing to check (leader witness is the bare path byte, delegator's is empty)
        check_token_input(1, &leader(program, None), OWNER_COVENANT_ID, true).unwrap();
        check_token_input(1, &delegator(program, None), OWNER_COVENANT_ID, false).unwrap();
    }
}

#[test]
fn malformed_token_witnesses_are_not_a_verdict_on_the_hash_type() {
    let program = TemplateId::Kcc20Ref8x8;
    let sig = proof(OWNER_P2PK_SCHNORR, SIGHASH_ALL);
    let lead = leader(program, Some(&sig));
    let deleg = delegator(program, Some(&sig));
    // a leader script read as a delegator and the reverse
    assert_eq!(token_owner_proof(&lead, false), None);
    assert_eq!(token_owner_proof(&deleg, true), None);
    // the borrow path (0x01) is not the owner path: flip the path byte in front of the proof
    let mut borrow = lead.clone();
    let proof_at = token_owner_proof(&lead, true).unwrap().as_ptr() as usize - lead.as_ptr() as usize;
    borrow[proof_at - 1] = 0x01;
    assert_eq!(token_owner_proof(&borrow, true), None);
    assert_eq!(check_token_input(0, &borrow, OWNER_P2PK_SCHNORR, true).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
    // an empty script, a bare redeem, a non-push opcode
    for bad in [vec![], push_data(&[1, 2, 3]), vec![0xac, 0xac, 0xac, 0xac]] {
        assert_eq!(token_owner_proof(&bad, true), None);
        assert_eq!(token_owner_proof(&bad, false), None);
        assert_eq!(check_token_input(0, &bad, OWNER_P2PK_SCHNORR, true).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
    }
    // a proof whose length does not match the scheme
    let e = check_token_input(0, &deleg, OWNER_P2PKH_SCHNORR, false).unwrap_err();
    assert_eq!(e.diag, Diag::InvalidKaspaExactSignature, "{e}");
    let short = delegator(program, Some(&sig[..64]));
    assert_eq!(check_token_input(0, &short, OWNER_P2PK_SCHNORR, false).unwrap_err().diag, Diag::InvalidKaspaExactSignature);
}

/// KCC-2 registry (kaspanet/kccs 411b41b `kcc-0002/vectors/authority-schemes.json`, vendored in kob-tests): the owner-proof
/// parser knows exactly the assigned schemes `0x00`-`0x04`; a reserved (`0x05`-`0x7f`) or custom (`0x80`-`0xff`) byte never
/// parses, whatever the proof length, and the owner-scheme constants are the canonical KCC-2 bytes.
#[test]
fn owner_proof_knows_exactly_the_assigned_kcc2_schemes() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../kob-tests/vectors/kcc2/authority-schemes.json")).expect("KCC-2 vectors");
    for c in v["constructions"].as_array().unwrap() {
        let b = u8::from_str_radix(c["scheme_byte"].as_str().unwrap(), 16).unwrap();
        let ours = match c["scheme"].as_str().unwrap() {
            "p2pk-schnorr/v1" => OWNER_P2PK_SCHNORR,
            "p2pkh-schnorr/v1" => OWNER_P2PKH_SCHNORR,
            "p2pkh-ecdsa/v1" => OWNER_P2PKH_ECDSA,
            "p2sh/v1" => OWNER_P2SH,
            "covenant-id/v1" => OWNER_COVENANT_ID,
            other => panic!("unknown scheme {other}"),
        };
        assert_eq!(ours, b, "{}", c["scheme"]);
    }
    for r in v["registry_checks"].as_array().unwrap() {
        let b = u8::from_str_radix(r["scheme_byte"].as_str().unwrap(), 16).unwrap();
        let parses = (0..=130usize).any(|len| owner_proof(b, &vec![0x01; len]).is_some());
        assert_eq!(parses, r["assigned"].as_bool().unwrap(), "scheme {b:#04x}");
    }
}
