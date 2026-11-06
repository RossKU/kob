//! The cancel-only table (`kob_protocol::retired`, `docs/spec/template-retirement.md`): the maker's cancel of an order under a
//! RETIRED template, executed in the rusty-kaspa script engine against that template's own bytecode.
//!
//! For every retired template of the table (the bid re-version `acf39cf`: `KobBid` b995661f and its KRON twins, the conditional,
//! if-done and pair kinds that embed it and their delivery-0 builds; the F1 generation `da35841`): an order of its kind, on a
//! program of its family, is cancelled with `CancelOrder::retired`. The order input is the retired template's P2SH (the bytes
//! of `crates/kob-protocol/data/retired`, read here from disk), its `cancel(sig)` validates, and the custody, the strays and
//! the order's KAS go back to the maker. A signature of anyone else, or the redeem script of another retired template of the
//! same kind, is refused by the engine; the builders refuse a replacement, records and a hash outside the table.
//!
//! Run: cargo test --release -p kob-tests --test retired_cancel_tests

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;

use fx::pair::*;
use fx::*;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::build::{build_cancel_order, build_cancel_position, CancelItem, CancelOrder, CancelPosition};
use kob_protocol::json::to_hex;
use kob_protocol::retired::{retired, retired_by_hash, RetiredTemplate};
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, sign_locally, spk_to_string, BuiltTx, FinalizeOptions, OrderUtxo, SigPlan, SignedTx, TokenUtxo};
use kob_protocol::verify::validate_signed;

/// The covenant id of the cancelled order.
const O: [u8; 32] = [0xa7; 32];

fn sign(built: &BuiltTx) -> SignedTx {
    finalize(built, &sign_locally(built, &keys()).unwrap(), FinalizeOptions::default()).unwrap()
}

/// The program an order of the retired kind is tested on: the pair kinds on the 8/8 reference (A and B), the delivery-0 KRON
/// builds on KForge (holders id_type 0), the other KRON kinds on KRON 2433, the KCC-20 kinds on the 8/8 reference.
fn program_of(kind: TemplateId) -> TemplateId {
    if kind.is_pair() {
        TemplateId::Kcc20Ref8x8
    } else if kind.is_delivery0() {
        TemplateId::KForgeToken1785
    } else if kind.family() == kob_protocol::Family::Kron {
        TemplateId::KronToken2433
    } else {
        TemplateId::Kcc20Ref8x8
    }
}

/// An order of `kind` on `p` (maker A) and the KAS it holds.
fn order_of(kind: TemplateId, p: TemplateId) -> (AnyState, u64) {
    let f = p.family();
    let (s, v) = match kind.base() {
        TemplateId::KobBid => {
            let b = bid(MAKER_A, P245, p);
            let v = b.escrow(10 * WHOLE, 3).unwrap() as u64;
            (AnyState::KobBid(b), v)
        }
        TemplateId::KobCondAsk => (AnyState::KobCondAsk(cond_ask(MAKER_A, p)), CARRIER),
        TemplateId::KobCondBid => {
            let c = cond_bid(MAKER_A, p);
            let v = c.escrow(2).unwrap() as u64;
            (AnyState::KobCondBid(c), v)
        }
        TemplateId::KobIfdBid => {
            let i = IfdBidState { min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, p) };
            let v = i.escrow().unwrap() as u64;
            (AnyState::KobIfdBid(i), v)
        }
        TemplateId::KobIfdAsk => {
            let i = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A, p) };
            let v = i.escrow(CARRIER as i64).unwrap() as u64;
            (AnyState::KobIfdAsk(i), v)
        }
        TemplateId::KobCondPair => return (AnyState::KobCondPair(cond_pair_bid(MAKER_A, p, p)), PV),
        TemplateId::KobIfdPair => {
            let i = ifd_pair(MAKER_A, false, p, p);
            let v = ifd_value(&i);
            return (AnyState::KobIfdPair(i), v);
        }
        other => panic!("no retired template of kind {}", other.name()),
    };
    (s.into_family(f), v)
}

/// The order's custody token UTXOs (owned by the order id), in record order.
fn custodies_of(s: &AnyState, p: TemplateId) -> Vec<TokenUtxo> {
    s.custodies().into_iter().enumerate().map(|(k, (token, amount))| tutxo(p, token, 71 + k as u8, amount, O, true)).collect()
}

/// The P2SH script public key of an order of this retired template, from the committed bytes alone: the file's bytecode with the
/// state spliced into the span today's kind has (the same layout).
fn spk_from_file(r: &RetiredTemplate, state: &[u8]) -> String {
    let file = common_root().join(format!("crates/kob-protocol/data/retired/{}.{}.bin", r.kind.name(), &r.hash_hex()[..8]));
    let bytes = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let cur = template(r.kind);
    let (off, len) = (cur.prefix.len(), cur.state_len);
    let redeem = [&bytes[..off], state, &bytes[off + len..]].concat();
    assert_eq!(silverscript_abi::template_hash(&bytes[..off], &bytes[off + len..]), r.hash, "{}", r.label());
    spk_to_string(&kaspa_txscript::pay_to_script_hash_script(&redeem))
}

fn common_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cancel(o: OrderUtxo<AnyState>, retired: Option<[u8; 32]>, custodies: Vec<TokenUtxo>, strays: Vec<TokenUtxo>) -> CancelOrder {
    let mut cs = custodies.into_iter();
    CancelOrder {
        retired,
        order: o,
        custody: cs.next(),
        prefund: cs.next(),
        strays,
        foreign: vec![],
        tokens: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

/// Token units the transaction delivers to the maker's key (each token's next states, counted once).
fn maker_units(built: &BuiltTx) -> i64 {
    let maker = pk(MAKER_A);
    let mut seen = std::collections::BTreeSet::new();
    let mut units = 0;
    for (p, inp) in built.plans.iter().zip(&built.tx.inputs) {
        match p {
            SigPlan::TokenLeader { next_states, .. } => {
                units +=
                    next_states.iter().filter(|s| s.owner == maker && s.owner_scheme == SCHEME_P2PK).map(|s| s.amount).sum::<i64>()
            }
            SigPlan::KronToken { next_states, .. } if seen.insert(inp.utxo.covenant_id) => {
                units += next_states
                    .iter()
                    .filter(|s| s.owner == maker && s.id_type != kob_protocol::family::KRON_TYPE_COVID)
                    .map(|s| s.amount)
                    .sum::<i64>()
            }
            _ => {}
        }
    }
    units
}

#[test]
fn every_retired_template_is_cancelled_by_its_maker_in_the_engine() {
    let maker_spk = format!("0000{}", to_hex(kob_protocol::script::p2pk_spk(&pk(MAKER_A)).script()));
    assert_eq!(retired().len(), 31);
    for r in retired() {
        let p = program_of(r.kind);
        let (state, value) = order_of(r.kind, p);
        let label = format!("{} on {}", r.label(), p.name());
        assert_eq!(state.template_id(), r.kind, "{label}: the fixture is of the retired template's kind");
        let custodies = custodies_of(&state, p);
        let held: i64 = custodies.iter().map(|c| c.state.amount()).sum();
        let req = cancel(order(70, value, O, 1_000, state.clone()), Some(r.hash), custodies, vec![]);
        let built = build_cancel_order(&req, &kob_protocol::budget::lookup).unwrap_or_else(|e| panic!("{label}: build: {e}"));
        // the order input spends the retired template's script, through its cancel only
        assert!(
            matches!(&built.plans[0], SigPlan::RetiredCancel { retired, template, .. } if *retired == r.hash && *template == r.kind),
            "{label}: {:?}",
            built.plans[0]
        );
        let spk = spk_from_file(r, &state.encode());
        assert_eq!(built.tx.inputs[0].utxo.script_public_key, spk, "{label}: the input is the retired template's P2SH");
        assert_ne!(spk, spk_to_string(&template(r.kind).spk(&state.encode())), "{label}");
        let signed = sign(&built);
        let v = validate_signed(&signed).unwrap_or_else(|e| panic!("{label}: engine: {e}\nroles {:?}", built.roles));
        assert!(v.mass.within_block_limits(), "{label}");
        // custody back to the maker, and the order's KAS (less the fee) to the maker's key
        assert_eq!(maker_units(&built), held, "{label}: the custody returns to the maker");
        let to_maker: u64 = built.tx.outputs.iter().filter(|o| o.script_public_key == maker_spk).map(|o| o.value).sum();
        assert!(to_maker + built.fee.fee >= value, "{label}: the order's KAS returns to the maker ({to_maker} of {value})");

        // a signature by anyone else is refused by the retired script (its cancel checks the maker's signature)
        let mut forged = signed.clone();
        forged.tx.inputs[0].signature_script[5] ^= 0x01;
        assert!(validate_signed(&forged).is_err(), "{label}: a forged cancel signature validates");
    }
}

#[test]
fn the_redeem_script_of_another_retired_template_does_not_spend_the_order() {
    // KobCondAsk of the bid re-version and of F1: the same kind and layout, different bytes
    let a = retired().iter().find(|r| r.label() == "KobCondAsk 770314aa").unwrap();
    let b = retired().iter().find(|r| r.label() == "KobCondAsk 02a34664").unwrap();
    let p = program_of(a.kind);
    let (state, value) = order_of(a.kind, p);
    // the order rests under template a; the cancel is built through template b, then pointed at the order's real output
    let req = cancel(order(70, value, O, 1_000, state.clone()), Some(b.hash), custodies_of(&state, p), vec![]);
    let mut built = build_cancel_order(&req, &kob_protocol::budget::lookup).unwrap();
    built.tx.inputs[0].utxo.script_public_key = spk_from_file(a, &state.encode());
    // the maker signs that transaction (the digests follow the spent output's script)
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in built.sign.iter_mut() {
        r.sighash = kob_protocol::tx::sighash(&tx, &entries, r.input_index);
    }
    let signed = sign(&built);
    assert!(validate_signed(&signed).is_err(), "another template's redeem script spends the order");
    // and through its own template it does
    let ok = build_cancel_order(
        &cancel(order(70, value, O, 1_000, state.clone()), Some(a.hash), custodies_of(&state, p), vec![]),
        &kob_protocol::budget::lookup,
    )
    .unwrap();
    validate_signed(&sign(&ok)).unwrap();
}

#[test]
fn strays_return_with_a_retired_cancel() {
    let r = retired().iter().find(|r| r.label() == "KobCondAsk 770314aa").unwrap();
    let p = program_of(r.kind);
    let (state, value) = order_of(r.kind, p);
    let custodies = custodies_of(&state, p);
    let held: i64 = custodies.iter().map(|c| c.state.amount()).sum();
    let strays = vec![tutxo(p, TOKEN_COV, 76, 3, O, true), tutxo(p, TOKEN_COV, 78, 4, O, true)];
    let built =
        build_cancel_order(&cancel(order(70, value, O, 1_000, state), Some(r.hash), custodies, strays), &kob_protocol::budget::lookup)
            .unwrap();
    validate_signed(&sign(&built)).unwrap();
    assert_eq!(maker_units(&built), held + 7);
}

#[test]
fn cancel_all_takes_retired_and_current_orders_of_one_token_in_one_transaction() {
    let p = TemplateId::Kcc20Ref8x8;
    let r = retired().iter().find(|r| r.label() == "KobBid b995661f").unwrap();
    let b = bid(MAKER_A, P245, p);
    let v = b.escrow(10 * WHOLE, 3).unwrap() as u64;
    let item = |tag: u8, c: u8, retired: Option<[u8; 32]>| CancelItem {
        retired,
        order: order(tag, v, cov(c), 1_000, AnyState::KobBid(b.clone())),
        custody: None,
        prefund: None,
        strays: vec![],
    };
    let req = CancelPosition {
        orders: vec![item(60, 0xb1, Some(r.hash)), item(61, 0xb2, Some(r.hash)), item(62, 0xb3, None)],
        funding: vec![],
        change: None,
        lock_time: 0,
        token_carrier: None,
        records: vec![],
        fee: fee(),
    };
    let built = build_cancel_position(&req, &kob_protocol::budget::lookup).unwrap();
    assert!(matches!(built.plans[0], SigPlan::RetiredCancel { .. }) && matches!(built.plans[1], SigPlan::RetiredCancel { .. }));
    assert!(matches!(built.plans[2], SigPlan::Entry { .. }));
    validate_signed(&sign(&built)).unwrap();
    // records ride on no cancel of a retired order
    let mut with_records = req.clone();
    with_records.records = vec![kob_protocol::payload::Record::Note { text: "x".into() }];
    assert!(build_cancel_position(&with_records, &kob_protocol::budget::lookup).is_err());
}

#[test]
fn nothing_but_a_plain_cancel_is_built_for_a_retired_template() {
    let r = retired_by_hash(&retired().iter().find(|r| r.label() == "KobBid b995661f").unwrap().hash).unwrap();
    let p = TemplateId::Kcc20Ref8x8;
    let (state, value) = order_of(r.kind, p);
    let base = cancel(order(70, value, O, 1_000, state.clone()), Some(r.hash), vec![], vec![]);
    let b = |c: &CancelOrder| build_cancel_order(c, &kob_protocol::budget::lookup);
    assert!(b(&base).is_ok());
    // a cancel-replace
    let mut rep = base.clone();
    rep.replace = Some(kob_protocol::build::Replacement { order: state.clone(), value, token_carrier: None, deadline: None });
    assert!(b(&rep).is_err());
    // records
    let mut rec = base.clone();
    rec.records = vec![kob_protocol::payload::Record::Note { text: "x".into() }];
    assert!(b(&rec).is_err());
    // a hash outside the table, and a retired template of another kind
    let mut unknown = base.clone();
    unknown.retired = Some([0x5a; 32]);
    assert!(b(&unknown).is_err());
    let mut other = base.clone();
    other.retired = Some(retired().iter().find(|r| r.label() == "KobCondBid 26042aff").unwrap().hash);
    assert!(b(&other).is_err());
    // a plan naming a hash outside the table derives no script
    let plan = SigPlan::RetiredCancel { template: r.kind, retired: [0x5a; 32], state: state.encode(), maker: pk(MAKER_A) };
    assert!(plan.check().is_err() && plan.redeem().is_none());
}
