//! Tests `KobCurveAsk` through the protocol library (`docs/design/curve-ask-spec.md`).
//!
//! Covered: the state codec, the gates, the KOB1 record, every builder shape on every KCC-20
//! program in the script engine, the creation inside the token's genesis (unbound, then bound by
//! its first fill), and the default table.

mod common;

use common::curve::*;
use common::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::build::{build, Action, Batch, Leg};
use kob_protocol::defaults::{curve_table, CurveSpec};
use kob_protocol::issue::{build_creation, FundingUtxo, GenesisCurve, Holder, IssueSpec};
use kob_protocol::payload::{self, Custody, Record, REC_GENESIS};
use kob_protocol::state::{AnyState, CurveAskState, TokenState, CURVE_MAX_LEVELS};
use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions, OrderUtxo, SignedTx, TokenUtxo, TxJson, Utxo};
use kob_protocol::verify::validate_signed;
use kob_protocol::Family;

fn run(name: &str, a: &Action) -> SignedTx {
    let built = build(a).unwrap_or_else(|e| panic!("{name}: build: {e}"));
    let sigs = sign_locally(&built, &keys()).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).unwrap_or_else(|e| panic!("{name}: finalize: {e}"));
    let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{name}: engine rejected: {e}"));
    println!("{name}: fee {} sompi ({:.5} KAS)", v.fee, v.fee as f64 / 1e8);
    signed
}

fn kcc20_programs() -> Vec<TemplateId> {
    TemplateId::TOKEN_PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20).collect()
}

#[test]
fn curve_state_codec_matches_the_compiler() {
    let t = template(TemplateId::KobCurveAsk);
    for levels in [1usize, 9, 10, 31, 32, 100, CURVE_MAX_LEVELS] {
        let s = curve(TemplateId::Kcc20Ref, levels);
        let enc = s.encode();
        // the compiler's own encoding of the same runtime state
        let mut v = std::collections::BTreeMap::new();
        for (k, x) in [
            ("maker", s.maker.to_vec().into()),
            ("tokenCovId", s.token_cov_id.to_vec().into()),
            ("tokenTplHash", s.token_tpl_hash.to_vec().into()),
            ("extensionCommitment", s.extension_commitment.to_vec().into()),
        ] {
            v.insert(k.to_string(), x);
        }
        for (k, x) in [
            ("tplPrefixLen", s.tpl_prefix_len),
            ("tplSuffixLen", s.tpl_suffix_len),
            ("scale", s.scale),
            ("minFill", s.min_fill),
            ("tip", s.tip),
            ("activeFrom", s.active_from),
            ("levelSize", s.level_size),
            ("total", s.total),
            ("amountLeft", s.amount_left),
        ] {
            v.insert(k.to_string(), silverscript_abi::ArtifactValue::Int(x));
        }
        v.insert(
            "table".into(),
            silverscript_abi::ArtifactValue::Array(s.table.iter().map(|p| silverscript_abi::ArtifactValue::Int(*p)).collect()),
        );
        let want = silverscript_abi::encode_runtime_state_script(&t.artifact, &t.contract().runtime_state, &v).unwrap();
        assert_eq!(enc, want, "{levels} levels: the hand codec is the compiler's");
        assert_eq!(CurveAskState::decode(&enc).unwrap(), s);
        assert_eq!(CurveAskState::from_redeem(&s.redeem()).unwrap(), s);
        let any = AnyState::decode(TemplateId::KobCurveAsk, &enc).unwrap();
        assert_eq!(any, AnyState::KobCurveAsk(s.clone()));
        assert_eq!(any.redeem(), s.redeem());
        // JSON round trip (the wasm surface)
        let j = serde_json::to_string(&any).unwrap();
        assert_eq!(serde_json::from_str::<AnyState>(&j).unwrap(), any);
    }
    // non-canonical spans are refused: a non-minimal table push, a partial entry, a trailing byte
    let s = curve(TemplateId::Kcc20Ref, 2);
    let mut e = s.encode();
    let at = kob_protocol::state::CURVE_TABLE_AT;
    let mut nonmin = e[..at].to_vec();
    nonmin.extend([0x4c, 16]);
    nonmin.extend_from_slice(&e[at + 1..]);
    assert!(CurveAskState::decode(&nonmin).is_err());
    e.push(0);
    assert!(CurveAskState::decode(&e).is_err());
    let mut part = s.encode();
    part[at] = 15;
    part.pop();
    assert!(CurveAskState::decode(&part).is_err());
    assert!(CurveAskState { table: vec![], ..s }.try_encode().is_err());
}

#[test]
fn curve_record_round_trips() {
    for levels in [1usize, 50, 100] {
        for unbound in [false, true] {
            let mut s = curve(TemplateId::Kcc20Ref, levels);
            if unbound {
                s.token_cov_id = [0; 32];
            }
            let st = AnyState::KobCurveAsk(s.clone());
            let r = Record::order(0, &st, Some(Custody { token_output: 1, extension_commitment: EXT }), None);
            let bytes = payload::encode(std::slice::from_ref(&r)).unwrap();
            let back = payload::decode(&bytes).unwrap().unwrap();
            assert_eq!(back.records, vec![r]);
            println!(
                "ORDER record of a {levels}-level curve{}: payload {} B (state {} B)",
                if unbound { " (unbound)" } else { "" },
                bytes.len(),
                s.encode().len()
            );
            // AMEND of the curve, and an amend that would leave it unbound is refused
            let a =
                Record::amend(0, 0, &AnyState::KobCurveAsk(CurveAskState { tip: TIP, token_cov_id: TOKEN_COV, ..s.clone() }), None);
            let ab = payload::encode(std::slice::from_ref(&a)).unwrap();
            assert_eq!(payload::decode(&ab).unwrap().unwrap().records, vec![a]);
            let prev = AnyState::KobCurveAsk(s.clone());
            assert!(payload::check_amend_terms(&prev, &AnyState::KobCurveAsk(CurveAskState { token_cov_id: [0; 32], ..s.clone() }))
                .is_err());
            assert!(payload::check_amend_terms(
                &prev,
                &AnyState::KobCurveAsk(CurveAskState { amount_left: s.amount_left - 1, token_cov_id: TOKEN_COV, ..s.clone() })
            )
            .is_err());
            assert!(payload::check_amend_terms(
                &prev,
                &AnyState::KobCurveAsk(CurveAskState {
                    table: s.table.iter().map(|p| p + 1).collect(),
                    token_cov_id: TOKEN_COV,
                    ..s.clone()
                })
            )
            .is_ok());
        }
    }
}

#[test]
fn curve_gates() {
    let ok = curve(TemplateId::Kcc20Ref, 10);
    assert!(ok.check_numbers().is_ok());
    let bad: Vec<(&str, CurveAskState)> = vec![
        ("no level", CurveAskState { table: vec![], ..ok.clone() }),
        ("1,001 levels", CurveAskState { table: vec![P250; CURVE_MAX_LEVELS + 1], ..ok.clone() }),
        ("levelSize 0", CurveAskState { level_size: 0, ..ok.clone() }),
        ("total above nLevels·levelSize", CurveAskState { total: 10 * WHOLE + 1, amount_left: 10 * WHOLE + 1, ..ok.clone() }),
        ("amountLeft above total", CurveAskState { amount_left: ok.total + 1, ..ok.clone() }),
        ("amountLeft 0", CurveAskState { amount_left: 0, ..ok.clone() }),
        ("a negative tip", CurveAskState { tip: -1, ..ok.clone() }),
        ("a price below the tip", CurveAskState { tip: ok.table[0] + 1, ..ok.clone() }),
        ("a zero price", CurveAskState { table: [vec![0], ok.table[1..].to_vec()].concat(), ..ok.clone() }),
        (
            "levelSize·max/scale at 2^62",
            CurveAskState { level_size: 1 << 40, total: 1 << 40, amount_left: 1 << 40, table: vec![1 << 32], ..ok.clone() },
        ),
        ("minFill 0", CurveAskState { min_fill: 0, ..ok.clone() }),
        ("activeFrom < 0", CurveAskState { active_from: -1, ..ok.clone() }),
        ("scale 3", CurveAskState { scale: 3, ..ok.clone() }),
    ];
    for (what, s) in bad {
        assert!(s.check_numbers().is_err(), "{what} passes the gates");
        if !s.table.is_empty() {
            assert!(kob_protocol::build::check_new_order(&AnyState::KobCurveAsk(s)).is_err(), "{what}");
        }
    }
    // several curves of one token, unbound: the gates allow both
    assert!(
        kob_protocol::build::check_new_order(&AnyState::KobCurveAsk(CurveAskState { token_cov_id: [0; 32], ..ok.clone() })).is_ok()
    );
    // a KRON program is refused
    let (h, p, x) = tok_fields(TemplateId::KronToken2433);
    let k = CurveAskState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: x, extension_commitment: [0; 32], ..ok };
    assert!(kob_protocol::build::check_new_order(&AnyState::KobCurveAsk(k)).is_err());
}

/// Every builder shape on every KCC-20 program, through the engine; the maker's payout is the covenant's walk.
#[test]
fn curve_builders_on_every_kcc20_program() {
    for p in kcc20_programs() {
        for levels in [10usize, 50] {
            for (name, a) in curve_scenarios(p, levels) {
                let name = format!("{name}@{}.l{levels}", p.name());
                let signed = run(&name, &a);
                if let Action::Batch(b) = &a {
                    let (k, s, n) = b
                        .legs
                        .iter()
                        .enumerate()
                        .find_map(|(k, l)| match l {
                            Leg::CurveAsk { order, amount, .. } => Some((k, order.state.clone(), *amount)),
                            _ => None,
                        })
                        .expect("a curve leg");
                    let pay = s.proceeds(n).unwrap();
                    let extra = if n == s.amount_left { 2 * carrier() } else { 0 };
                    assert!(signed.tx.outputs[k].value >= pay as u64 + extra, "{name}: the maker's payout");
                }
                // a signed amend reveals the curve's previous state (its redeem script has the table's length)
                if let Action::AmendOrder(r) = &a {
                    let got = payload::recover_amends(&signed.tx).unwrap_or_else(|e| panic!("{name}: recover the amend: {e}"));
                    assert_eq!(got.len(), 1, "{name}");
                    assert_eq!(got[0].order, r.amended, "{name}");
                }
            }
        }
    }
}

/// A token created with its curve in one genesis: the creation verifies, the genesis payload carries the ORDER record
/// and the token states, the record recovers with its token derived, and the first fill binds the curve.
#[test]
fn curve_created_in_its_token_genesis() {
    let creator = pk(MAKER_A);
    let program = kob_protocol::issue::IssueProgram::PublicMint;
    let tpl = program.template_id();
    let (h, pl, sl) = tok_fields(tpl);
    let decimals = 3u8;
    let supply = 1_000 * WHOLE as u64;
    let table = curve_table(&CurveSpec { amount: 600 * WHOLE, ..CurveSpec::defaults(600 * WHOLE, SCALE) }).unwrap();
    let state = CurveAskState {
        maker: creator,
        token_cov_id: [0; 32],
        token_tpl_hash: h,
        tpl_prefix_len: pl,
        tpl_suffix_len: sl,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: table.level_size,
        total: 600 * WHOLE,
        amount_left: 600 * WHOLE,
        extension_commitment: kob_protocol::issue::EXTENSION_FIXED_SUPPLY,
        table: table.table.clone(),
    };
    let funding = vec![FundingUtxo {
        outpoint: kaspa_consensus_core::tx::TransactionOutpoint {
            transaction_id: kaspa_consensus_core::tx::TransactionId::from_bytes([0x5a; 32]),
            index: 0,
        },
        amount: 1_000 * KAS,
        owner_pubkey: creator,
    }];
    let mut spec = IssueSpec::new(
        "Curve Token",
        "CURVE",
        decimals,
        supply,
        vec![Holder::new([0; 32], 4, 600 * WHOLE as u64), Holder::new(creator, 0, 400 * WHOLE as u64)],
        funding,
    );
    spec.program = program;
    spec.curve = Some(GenesisCurve { state: state.clone(), custody: 0, carrier: carrier() });
    let mut c = build_creation(&spec).unwrap_or_else(|e| panic!("creation: {e}"));
    c.verify().unwrap_or_else(|e| panic!("verify: {e}"));
    let mut sk = [0u8; 32];
    sk.copy_from_slice(&keys()[&creator]);
    c.sign(&secp256k1::SecretKey::from_slice(&sk).unwrap()).unwrap();
    let report = c.verify().unwrap_or_else(|e| panic!("signed verify: {e}"));
    let g = &c.genesis;
    let placed = g.curve.clone().expect("the curve");
    println!(
        "genesis with a {}-level curve: {} B, fee {} sompi, storage mass {}; preparation fee {} sompi",
        state.levels(),
        report.size,
        g.fee,
        report.storage_mass,
        c.prep.fee
    );
    // the payload: KOB1 with the ORDER record and the token states
    let p = payload::decode(&g.tx.payload).unwrap().expect("KOB1");
    let states = p.records.iter().find_map(|r| match r {
        Record::Unknown { record_type, value } if *record_type == REC_GENESIS => Some(value.clone()),
        _ => None,
    });
    assert_eq!(kob_protocol::genesis_states::decode(&states.expect("GENESIS record")).unwrap().len(), 2);
    let rec = payload::recover_orders(&TxJson::from_tx(&g.tx, &g.entries)).unwrap_or_else(|e| panic!("recover: {e}"));
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].order, AnyState::KobCurveAsk(state.clone()));
    assert_eq!(rec[0].covenant_id, placed.covenant_id.as_bytes());
    // the first fill: the custody and the order from the genesis transaction
    let gid = g.tx.id().as_bytes();
    let token = g.covenant_id.as_bytes();
    let order = OrderUtxo {
        utxo: Utxo {
            transaction_id: gid,
            index: placed.output as u32,
            amount: carrier(),
            block_daa_score: 1_000,
            covenant_id: Some(placed.covenant_id.as_bytes()),
        },
        state: state.clone(),
    };
    let custody = TokenUtxo {
        utxo: Utxo { transaction_id: gid, index: 0, amount: carrier(), block_daa_score: 1_000, covenant_id: Some(token) },
        state: TokenState::custody(Family::Kcc20, state.total, placed.covenant_id.as_bytes(), state.extension_commitment),
    };
    let mut b: Batch = take_curve(&state, 1);
    b.legs = vec![Leg::CurveAsk { order: order.clone(), custody: custody.clone(), amount: 5 * WHOLE }];
    let signed = run("first fill of the unbound curve", &Action::Batch(b.clone()));
    let bound = CurveAskState { token_cov_id: token, amount_left: state.total - 5 * WHOLE, ..state.clone() };
    assert!(
        signed.tx.outputs.iter().any(|o| o.script_public_key == kob_protocol::tx::spk_to_string(&bound.spk())),
        "the continuation is bound"
    );
    // a custody from another transaction is refused by the builder
    let mut other = b.clone();
    if let Leg::CurveAsk { custody, .. } = &mut other.legs[0] {
        custody.utxo.transaction_id = [0x77; 32];
    }
    assert!(build(&Action::Batch(other)).is_err());
    // the maker's cancel of the unbound curve
    let mut cancel = cancel_curve(&state);
    cancel.order = OrderUtxo { utxo: order.utxo.clone(), state: AnyState::KobCurveAsk(state.clone()) };
    cancel.custody = Some(custody.clone());
    let signed = run("cancel of the unbound curve", &Action::CancelOrder(cancel.clone()));
    // the custody's tokens go back to the maker under their own token's covenant id
    let back = TokenState::user(tpl, state.total, creator, state.extension_commitment);
    let back_spk = kob_protocol::tx::spk_to_string(&back.spk_with(kob_protocol::artifacts::token_template(tpl)));
    assert!(
        signed
            .tx
            .outputs
            .iter()
            .any(|o| o.script_public_key == back_spk && o.covenant.as_ref().is_some_and(|c| c.covenant_id == token)),
        "the custody's tokens return to the maker"
    );
    // its token is the custody's: none, or one from another transaction, is refused
    let mut none = cancel.clone();
    none.custody = None;
    assert!(build(&Action::CancelOrder(none)).unwrap_err().to_string().contains("needs its custody"));
    let mut other = cancel;
    other.custody = Some(TokenUtxo { utxo: Utxo { transaction_id: [0x77; 32], ..custody.utxo.clone() }, ..custody });
    assert!(build(&Action::CancelOrder(other)).unwrap_err().to_string().contains("output of its genesis"));
}

#[test]
fn curve_default_table() {
    for (amount, levels, total, mult) in [
        (1_000_000 * WHOLE, 100usize, 1_000_000_000_000i64, 5_000u64),
        (21_000_000 * WHOLE, 100, 1_000_000_000_000, 5_000),
        (1_000 * WHOLE, 1, 50 * KAS as i64, 1_000),
        (12_345, 100, 123_456_789, 2_500),
        (7, 100, 70 * KAS as i64, 5_000),
        (100 * WHOLE, 1_000, 10_000 * KAS as i64, 10_000),
    ] {
        let spec = CurveSpec { amount, levels, total, multiple_milli: mult, scale: SCALE, ..CurveSpec::defaults(amount, SCALE) };
        let t = curve_table(&spec).unwrap();
        let n = t.table.len();
        let sizes: Vec<i64> = (0..n).map(|i| if i + 1 < n { t.level_size } else { amount - (n as i64 - 1) * t.level_size }).collect();
        let real: i64 = sizes
            .iter()
            .zip(&t.table)
            .map(|(s, p)| kob_protocol::state::quote_of(*s, *p, SCALE, kob_protocol::state::Round::Up).unwrap())
            .sum();
        assert_eq!(real, t.realised);
        assert!(t.realised <= total && t.table.windows(2).all(|w| w[0] <= w[1]));
        // as close as whole prices allow: one more sompi on the top level overshoots
        let top = quote_of_up(sizes[n - 1], t.table[n - 1] + 1) - quote_of_up(sizes[n - 1], t.table[n - 1]);
        assert!(t.realised + top > total || top == 0, "{amount} {levels}: {} of {total} with room on the top level", t.realised);
        let ratio = *t.table.last().unwrap() as f64 / t.table[0] as f64;
        println!(
            "{amount} in {levels} levels for {total}: {n} levels of {}, {} .. {} (x{ratio:.4}), realised {} ({} short)",
            t.level_size,
            t.table[0],
            t.table[n - 1],
            t.realised,
            total - t.realised
        );
        // deterministic
        assert_eq!(curve_table(&spec).unwrap(), t);
    }
    // the UI defaults: 1,000,000 tokens, 100 levels, 10,000 KAS, x5
    let t = curve_table(&CurveSpec::defaults(1_000_000 * WHOLE, SCALE)).unwrap();
    assert_eq!(t.table.len(), 100);
    let x = *t.table.last().unwrap() as f64 / t.table[0] as f64;
    assert!((x - 5.0).abs() < 0.001, "x{x}");
    assert!(curve_table(&CurveSpec { multiple_milli: 999, ..CurveSpec::defaults(10, SCALE) }).is_err());
    assert!(curve_table(&CurveSpec { levels: 0, ..CurveSpec::defaults(10, SCALE) }).is_err());
}

fn quote_of_up(n: i64, p: i64) -> i64 {
    kob_protocol::state::quote_of(n, p, SCALE, kob_protocol::state::Round::Up).unwrap()
}

/// A refund request that names a curve is refused cleanly (a curve has no refund).
#[test]
fn sec_refund_request_of_a_curve_is_refused() {
    use kob_protocol::build::RefundOrder;
    let c = cancel_curve(&curve(TemplateId::Kcc20Ref, 10));
    let a = Action::RefundOrder(RefundOrder {
        order: c.order.clone(),
        custody: c.custody.clone(),
        prefund: None,
        bought_custody: None,
        foreign: vec![],
        lock_time: NOW,
        funding: vec![],
        change: None,
        fee: fee(),
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(&a).map(|_| ()).map_err(|e| e.to_string())));
    let e = r.expect("no panic").expect_err("refused");
    assert!(e.contains("no refund"), "{e}");
}

/// The default path: the curve default minFill is at most one level, so a small curve of the
/// default shape takes fills from any position; the uncapped default (more than 8 levels) is
/// refused by the gates.
#[test]
fn sec_default_min_fill_of_a_small_curve_is_within_one_level() {
    use kob_protocol::defaults::{curve_min_fill, default_min_fill};
    let amount = 1_000_000 * WHOLE;
    let spec = CurveSpec { total: (200 * KAS) as i64, ..CurveSpec::defaults(amount, SCALE) };
    let t = curve_table(&spec).unwrap();
    let base = CurveAskState {
        level_size: t.level_size,
        total: amount,
        amount_left: amount,
        table: t.table.clone(),
        ..curve(TemplateId::Kcc20Ref, 1)
    };
    let old = CurveAskState { min_fill: default_min_fill(amount, t.table[0], SCALE), ..base.clone() };
    assert!(old.min_fill > 8 * old.level_size);
    assert!(old.check_numbers().is_err(), "a minFill beyond what one fill reaches is refused");
    assert!(kob_protocol::build::check_new_order(&AnyState::KobCurveAsk(old)).is_err());
    let s = CurveAskState { min_fill: curve_min_fill(amount, t.table[0], t.level_size, SCALE), ..base };
    assert!(s.min_fill <= s.level_size);
    s.check_numbers().expect("the capped default passes");
    assert!(s.fill_ok(s.max_fill()) && s.fill_ok(s.min_fill));
    // the gate's bound: 7 levels + 1 passes, one more is refused
    let at = |m: i64| CurveAskState { min_fill: m, ..s.clone() }.check_numbers();
    assert!(at(7 * s.level_size + 1).is_ok() && at(7 * s.level_size + 2).is_err());
}

/// A level priced at the tip (it would sell for nothing) is refused.
#[test]
fn sec_a_level_at_the_tip_is_refused() {
    let s = curve(TemplateId::Kcc20Ref, 10);
    assert!(CurveAskState { tip: s.table[0], ..s.clone() }.check_numbers().is_err());
    assert!(CurveAskState { tip: s.table[0] - 1, ..s.clone() }.check_numbers().is_ok());
}

/// The amend of an unbound curve names the token of its custody, which the builder is given and
/// checks.
#[test]
fn sec_unbound_amend_names_its_custodys_token() {
    let mut s = curve(TemplateId::Kcc20Ref, 10);
    s.token_cov_id = [0; 32];
    let mut a = amend_curve(&s);
    let bound = CurveAskState { token_cov_id: TOKEN_COV, tip: TIP, ..s.clone() };
    a.amended = AnyState::KobCurveAsk(bound.clone());
    // no custody: refused
    assert!(build(&Action::AmendOrder(a.clone())).unwrap_err().to_string().contains("needs its custody"));
    let c = cancel_curve(&s);
    let mut custody = c.custody.clone().expect("a custody");
    custody.utxo.transaction_id = a.order.utxo.transaction_id;
    custody.utxo.covenant_id = Some(TOKEN_COV);
    a.custody = Some(custody.clone());
    build(&Action::AmendOrder(a.clone())).expect("bound to its custody's token");
    // another token
    a.amended = AnyState::KobCurveAsk(CurveAskState { token_cov_id: [0x77; 32], ..bound });
    assert!(build(&Action::AmendOrder(a.clone())).unwrap_err().to_string().contains("token of its custody"));
}

/// The chromatic shape: one semitone per level, `12·octaves + 1` levels, every 12 levels exactly
/// double the price, the top `2^octaves` times the first; the total sets the start; the edges.
/// The vector is the one the web test checks through kob-wasm (`web/src/kob/curve.test.ts`).
#[test]
fn chromatic_tables_climb_one_semitone_per_level() {
    use kob_protocol::defaults::{CurveShape, SEMITONES};
    for octaves in 1..=8u32 {
        let spec = CurveSpec::chromatic(1_000_000 * 100_000_000, 100_000_000, octaves);
        assert_eq!(spec.shape, CurveShape::Chromatic);
        let t = curve_table(&spec).unwrap();
        assert_eq!(t.table.len(), 12 * octaves as usize + 1);
        for i in 12..t.table.len() {
            assert_eq!(t.table[i], 2 * t.table[i - 12], "octave {octaves} level {i}");
        }
        assert_eq!(*t.table.last().unwrap(), t.table[0] << octaves);
        assert!(t.table.windows(2).all(|w| w[0] < w[1]));
        // a semitone up, within the integer rounding of the start
        for (k, s) in SEMITONES.iter().enumerate().skip(1) {
            let want = (t.table[0] as u128 * s) >> 40;
            assert!((t.table[k] as i128 - want as i128).abs() <= (s >> 40) as i128 + 1, "level {k}");
        }
        assert!(t.realised <= spec.total && t.realised > spec.total - spec.total / 1000, "{} of {}", t.realised, spec.total);
    }
    let v = curve_table(&CurveSpec::chromatic(2_500_000_000, 100, 1)).unwrap();
    assert_eq!((v.level_size, v.realised), (192_307_693, VECTOR_REALISED));
    assert_eq!(v.table, VECTOR);
    // edges: 0 and 9 octaves, fewer base units than levels (fewer levels, the octaves still exact where they reach)
    assert!(curve_table(&CurveSpec::chromatic(1_000, 100, 0)).is_err());
    assert!(curve_table(&CurveSpec::chromatic(1_000, 100, 9)).is_err());
    let small = curve_table(&CurveSpec::chromatic(5, 1, 1)).unwrap();
    assert_eq!(small.table.len(), 5);
    assert!(curve_table(&CurveSpec { total: 1, ..CurveSpec::chromatic(1_000_000, 1, 8) }).is_err(), "a start below 1 sompi");
}

const VECTOR_REALISED: i64 = 999_999_998_632;
const VECTOR: [i64; 13] = [27_634, 29_278, 31_018, 32_863, 34_817, 36_888, 39_081, 41_405, 43_867, 46_475, 49_239, 52_167, 55_268];

/// P-L4: a curve state with no level, or more levels than one push holds, is an error wherever it
/// comes in (an indexer's JSON, a builder request), never a panic of the cancel builder or of the
/// address computation (a trap in kob-wasm).
#[test]
fn sec_an_empty_or_unencodable_curve_table_is_an_error_not_a_panic() {
    let s = curve(TemplateId::Kcc20Ref, 4);
    let json = serde_json::to_value(Action::CancelOrder(cancel_curve(&s))).unwrap();
    assert!(serde_json::from_value::<Action>(json.clone()).is_ok(), "the request as it is reads");
    for table in [serde_json::json!([]), serde_json::Value::Array(vec![serde_json::json!(P250.to_string()); 65_535 / 8 + 1])] {
        let mut v = json.clone();
        v["order"]["state"]["state"]["table"] = table;
        let e = serde_json::from_value::<Action>(v).expect_err("a table no push holds is refused as it is read");
        assert!(e.to_string().contains("table"), "{e}");
    }
    // a state made in code: its cancel is an error too
    let empty = CurveAskState { table: vec![], ..s };
    let e = build(&Action::CancelOrder(cancel_curve(&empty))).expect_err("the cancel of an empty curve is refused");
    assert!(e.to_string().contains("table"), "{e}");
}
