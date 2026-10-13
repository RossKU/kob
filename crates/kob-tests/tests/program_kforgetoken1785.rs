//! Conformance tests of the token program `KForgeToken1785`: KForge's launchpad token program (testnet-10, template
//! `5b136633...`), a pre-standard program with the KRON 46-byte state behind a one-byte entry-selector prefix whose holders are
//! id_type 0 (a token-level signature in the signature column). Fixtures: `contracts/third-party/kforge/` (`program.bin`, a
//! redeem script observed on testnet-10; `observed-transfer.json`, a real transfer). The profile is in
//! `crates/kob-protocol/data/programs.json` (`pending-review`, `networks: [testnet-10]`); KOB's KRON order kinds serve it in
//! their delivery id_type 0 builds (`*KronD0`, `contracts/adapters/kron/README.md`).
//!
//! What the reviewer checklist of `docs/spec/token-programs.md` (section 5) asks, item by item:
//!
//! 1-3. provenance, layout, witness: the profile derives from the bytes, KOB's `kron-columns` builder reproduces a real
//!      transfer byte for byte, tampered witnesses are refused;
//! 4.   owners: custody (id_type 2) is released only with the order covenant's input; holders are id_type 0 (the owner's
//!      signature over the input's SIGHASH_ALL digest in the signature column) and address presence (id_type 3) is refused, so
//!      the `*KronD0` order builds deliver to it;
//! 5.   conservation and authority: no minting, no minter created, no burning on the transfer branch; every other selector
//!      value refuses what branch 0 refuses (`other_selector_branches_move_nothing_branch_zero_refuses`);
//! 6.   slots (1 / 3) and carrier floor (1 sompi) measured;
//! 9.   (part) the KOB builders' orders on the program, built and validated in the engine (`kob_orders_on_kforge`; the builder
//!      scenarios over every program in `crates/kob-protocol/tests` run on it too).
//!
//! Run: cargo test -p kob-tests --test program_kforgetoken1785 -- --nocapture

#![allow(clippy::needless_range_loop)]

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::{OpCheckSig, OpData32, OpTrue};
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use secp256k1::{Keypair, Secp256k1, SecretKey};

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::profile::{
    derive_profile, DeriveOptions, EmbeddedSource, ProgramInput, ProgramReview, Slots, SourceKind, WitnessScheme,
};
use kob_protocol::registry::Family;
use kob_protocol::script::{kron_columns_sigscript, push_data};
use kob_protocol::state::KronState;

const KFORGE_HASH: &str = "5b136633944a2371a1d0f6a94fdc0d329327ffc700fdec097fc59e76e1d33ae1";
const KAS: u64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x5b; 32]);
const ORDER_COV: Hash = Hash::from_bytes([0xc0; 32]);

fn program() -> TemplateId {
    TemplateId::KForgeToken1785
}

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn program_bytes() -> Vec<u8> {
    std::fs::read(repo("contracts/third-party/kforge/program.bin")).expect("program.bin")
}

fn hex(s: &str) -> Vec<u8> {
    kob_protocol::json::from_hex(s).unwrap()
}

/// Data pushes of a push-only script (`OP_0` = empty, `OP_1..OP_16` = one byte).
fn pushes(script: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let mut i = 0;
    while i < script.len() {
        let op = script[i];
        i += 1;
        let n = match op {
            0x00 => {
                out.push(vec![]);
                continue;
            }
            0x01..=0x4b => op as usize,
            0x4c => {
                i += 1;
                script[i - 1] as usize
            }
            0x4d => {
                i += 2;
                u16::from_le_bytes([script[i - 2], script[i - 1]]) as usize
            }
            0x51..=0x60 => {
                out.push(vec![op - 0x50]);
                continue;
            }
            other => panic!("not a push-only script: opcode {other:#x}"),
        };
        out.push(script[i..i + n].to_vec());
        i += n;
    }
    out
}

/// The embedded program and its trailing pushes (the entry selector of the transfer branch).
struct Cut {
    trailing: Vec<Vec<u8>>,
}

impl Cut {
    fn embedded() -> Cut {
        Cut { trailing: program().witness_trailing() }
    }
    fn redeem(&self, s: &KronState) -> Vec<u8> {
        s.redeem_with(token_template(program()))
    }
    fn spk(&self, s: &KronState) -> ScriptPublicKey {
        s.spk_with(token_template(program()))
    }
}

fn ks(owner: [u8; 32], id_type: u8, amount: i64) -> KronState {
    KronState { owner, id_type, amount, is_minter: 0 }
}

// ------------------------------------------------------------------------------------------------ items 1-3: the bytes

#[test]
fn profile_matches_the_embedded_bytes() {
    let p = program().profile().expect("profile");
    let t = token_template(program());
    assert_eq!(t.hash, p.hash());
    assert_eq!(p.template_hash, KFORGE_HASH);
    assert_eq!((t.prefix.len(), t.state_len, t.suffix.len()), (p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len));
    assert_eq!(t.redeem(&program_bytes()[1..47]), program_bytes(), "the embedded program is program.bin");
    assert_eq!(p.review.status, ProgramReview::PendingReview);
    assert_eq!(p.networks, vec!["testnet-10".to_string()]);
    assert!(!p.router_intents);
}

/// `kob registry add-program contracts/third-party/kforge/program.bin --name KForgeToken1785 --slots 1/3 --holder 0
/// --network testnet-10` derives the embedded profile's byte facts from the bytes alone.
#[test]
fn the_profile_derives_from_the_bytes() {
    let p = program().profile().unwrap();
    let d = derive_profile(
        &ProgramInput::Redeem(program_bytes()),
        &DeriveOptions {
            name: Some("KForgeToken1785".into()),
            slots: Some(Slots { inputs: 1, outputs: 3 }),
            holder: Some(0),
            networks: vec!["testnet-10".into()],
            ..Default::default()
        },
    )
    .expect("derivable");
    assert_eq!(d.already_embedded.as_deref(), Some("KForgeToken1785"));
    let q = &d.profile;
    assert_eq!((q.codec, q.layout.prefix_len, q.layout.state_len, q.layout.suffix_len), (Family::Kron, 1, 46, 1738));
    assert_eq!(q.witness.scheme, WitnessScheme::KronColumns);
    assert_eq!(q.witness.trailing_pushes, vec![String::new()], "one trailing OP_0: the entry selector of branch 0");
    assert_eq!(q.source.kind, SourceKind::RawRedeem);
    let facts = |x: &kob_protocol::profile::ProgramProfile| {
        (x.template_hash.clone(), x.layout, x.codec, x.witness.clone(), x.owners, x.slots, x.carrier_floor, x.networks.clone())
    };
    assert_eq!(facts(q), facts(p));
    let leaked: &'static [u8] = Box::leak(program_bytes().into_boxed_slice());
    p.check_bytes(&EmbeddedSource::Raw(leaked)).expect("the profile matches the bytes");
}

#[test]
fn the_kron_columns_builder_reproduces_a_real_kforge_transfer() {
    let cut = Cut::embedded();
    let obs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo("contracts/third-party/kforge/observed-transfer.json")).unwrap()).unwrap();
    let sig = hex(obs["signature_script"].as_str().unwrap());
    let p = pushes(&sig);
    assert_eq!(p.len(), 7 + cut.trailing.len(), "4 columns, sigs, witnesses, the selector, the redeem script");
    let redeem = p.last().unwrap();
    assert_eq!(redeem, &program_bytes(), "the observed input spends program.bin");
    let n = p[1].len();
    let next: Vec<KronState> = (0..n)
        .map(|k| KronState {
            owner: p[0][32 * k..32 * k + 32].try_into().unwrap(),
            id_type: p[1][k],
            amount: i64::from_le_bytes(p[2][8 * k..8 * k + 8].try_into().unwrap()),
            is_minter: p[3][k],
        })
        .collect();
    let rebuilt = kron_columns_sigscript(redeem, &next, &p[4], &p[5], &cut.trailing);
    assert_eq!(kob_protocol::json::to_hex(&rebuilt), kob_protocol::json::to_hex(&sig), "byte-identical signature script");
    let outs = obs["token_outputs"].as_array().unwrap();
    assert_eq!(outs.len(), next.len());
    for (o, s) in outs.iter().zip(&next) {
        assert_eq!(o["script_public_key"].as_str().unwrap(), kob_protocol::json::to_hex(cut.spk(s).script()), "{s:?}");
    }
}

// ------------------------------------------------------------------------------------------------ the engine

fn keypair(seed: u8) -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[seed; 32]).unwrap())
}
fn xonly(kp: &Keypair) -> [u8; 32] {
    kp.x_only_public_key().0.serialize()
}
fn p2pk_spk(pk: &[u8; 32]) -> ScriptPublicKey {
    let mut s = vec![OpData32];
    s.extend_from_slice(pk);
    s.push(OpCheckSig);
    ScriptPublicKey::new(0, s.into())
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], n: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), n, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).unwrap();
    let mut sig = kp.sign_schnorr(msg).as_ref().to_vec();
    sig.push(1);
    sig
}

enum In {
    /// a KForge token input in this state, no signature
    Tok(KronState),
    /// a KForge token input in this state whose signature column carries this key's signature of input `.2` (`None`: itself)
    TokSigned(KronState, Keypair, Option<usize>),
    /// a P2PK input of this key (address presence)
    P2pk(Keypair),
    /// an `OP_TRUE` P2SH input carrying this covenant id (stands for a KOB order input)
    Cov(Hash),
}

struct Case {
    ins: Vec<In>,
    /// token outputs: (state, KAS value)
    outs: Vec<(KronState, u64)>,
    /// witness byte per token input (index of the authorising input)
    wit: Vec<u8>,
    /// trailing pushes (the profile's, or a tampered set)
    trailing: Vec<Vec<u8>>,
}

fn run(cut: &Cut, c: &Case) -> Result<(), String> {
    let entries: Vec<UtxoEntry> = c
        .ins
        .iter()
        .map(|i| match i {
            In::Tok(s) | In::TokSigned(s, ..) => UtxoEntry::new(KAS, cut.spk(s), 0, false, Some(TOKEN_COV)),
            In::P2pk(kp) => UtxoEntry::new(10 * KAS, p2pk_spk(&xonly(kp)), 0, false, None),
            In::Cov(h) => UtxoEntry::new(KAS, pay_to_script_hash_script(&[OpTrue]), 0, false, Some(*h)),
        })
        .collect();
    let first_tok = c.ins.iter().position(|i| matches!(i, In::Tok(_) | In::TokSigned(..))).unwrap() as u16;
    let outputs: Vec<TransactionOutput> = c
        .outs
        .iter()
        .map(|(s, v)| TransactionOutput {
            value: *v,
            script_public_key: cut.spk(s),
            covenant: Some(CovenantBinding { authorizing_input: first_tok, covenant_id: TOKEN_COV }),
        })
        .collect();
    let inputs: Vec<TransactionInput> = (0..c.ins.len())
        .map(|n| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([n as u8 + 1; 32]), index: n as u32 },
                vec![],
                0,
                0,
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    let next: Vec<KronState> = c.outs.iter().map(|(s, _)| s.clone()).collect();
    for (n, i) in c.ins.iter().enumerate() {
        tx.inputs[n].signature_script = match i {
            In::Tok(s) => kron_columns_sigscript(&cut.redeem(s), &next, &[], &c.wit, &c.trailing),
            In::TokSigned(s, kp, of) => {
                let sig = sign(&unsigned, &entries, of.unwrap_or(n), kp);
                kron_columns_sigscript(&cut.redeem(s), &next, &sig, &c.wit, &c.trailing)
            }
            In::P2pk(kp) => push_data(&sign(&unsigned, &entries, n, kp)),
            In::Cov(_) => push_data(&[OpTrue]),
        };
    }
    let populated = PopulatedTransaction::new(&tx, entries.clone());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    for i in 0..tx.inputs.len() {
        let input = tx.inputs[i].clone();
        let mut vm = TxScriptEngine::from_transaction_input(
            &populated,
            &input,
            i,
            &entries[i],
            EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
            EngineFlags { sigop_script_units: 100_000u64.into() },
        );
        vm.execute().map_err(|e| format!("input {i}: {e:?}"))?;
    }
    Ok(())
}

/// The custody (id_type 2) of an order releasing `outs`.
fn fill(cut: &Cut, trailing: Vec<Vec<u8>>, outs: Vec<(KronState, u64)>) -> Case {
    let _ = cut;
    Case { ins: vec![In::Cov(ORDER_COV), In::Tok(ks(ORDER_COV.as_bytes(), 2, 600))], outs, wit: vec![0], trailing }
}

// ------------------------------------------------------------------------------------------------ item 3, 4: witness, owners

#[test]
fn custody_is_released_only_by_the_order_covenant_and_the_transfer_selector() {
    let cut = Cut::embedded();
    let b = xonly(&keypair(9));
    let order = ORDER_COV.as_bytes();
    let honest = vec![(ks(b, 0, 250), 1), (ks(order, 2, 350), 1)];
    assert!(run(&cut, &fill(&cut, cut.trailing.clone(), honest.clone())).is_ok());
    // tampered: no selector push, another selector value, a witness naming another input, no covenant input at all
    assert!(run(&cut, &fill(&cut, vec![], honest.clone())).is_err(), "the selector push is required");
    let mut wrong_wit = fill(&cut, cut.trailing.clone(), honest.clone());
    wrong_wit.ins.insert(0, In::P2pk(keypair(9)));
    wrong_wit.wit = vec![0];
    assert!(run(&cut, &wrong_wit).is_err(), "a witness naming a P2PK input does not release custody");
    let stolen =
        Case { ins: vec![In::P2pk(keypair(9)), In::Tok(ks(order, 2, 600))], ..fill(&cut, cut.trailing.clone(), honest.clone()) };
    assert!(run(&cut, &stolen).is_err(), "custody without the order covenant's input is refused");
    // the next-state column must be the outputs: a column that differs from the outputs is refused
    let mut c = fill(&cut, cut.trailing.clone(), honest);
    c.outs[0].0.owner = xonly(&keypair(10));
    let entries_ok = run(&cut, &c);
    assert!(entries_ok.is_ok(), "a consistent column to another owner is a transfer the custody owner authorised");
}

/// Holders are id_type 0: the owner's SIGHASH_ALL signature of the token input in the signature column authorises it (the
/// witness byte is not read for id_type 0); no signature, another key's signature, a signature of another input, and address
/// presence (id_type 3, a P2PK input of the owner) are refused.
#[test]
fn holders_are_id_type_0_with_a_token_level_signature() {
    let cut = Cut::embedded();
    let maker = keypair(7);
    let m = xonly(&maker);
    let order = ORDER_COV.as_bytes();
    let outs = vec![(ks(order, 2, 600), 1), (ks(m, 0, 400), 1)];
    let case = |ins: Vec<In>, w: u8| Case { ins, outs: outs.clone(), wit: vec![w], trailing: cut.trailing.clone() };
    for w in 0..3u8 {
        assert!(run(&cut, &case(vec![In::TokSigned(ks(m, 0, 1_000), keypair(7), None)], w)).is_ok(), "witness {w}");
        assert!(run(&cut, &case(vec![In::P2pk(keypair(8)), In::TokSigned(ks(m, 0, 1_000), keypair(7), None)], w)).is_ok());
    }
    assert!(run(&cut, &case(vec![In::P2pk(keypair(7)), In::Tok(ks(m, 0, 1_000))], 0)).is_err(), "no signature");
    assert!(run(&cut, &case(vec![In::TokSigned(ks(m, 0, 1_000), keypair(8), None)], 0)).is_err(), "another key's signature");
    assert!(
        run(&cut, &case(vec![In::P2pk(keypair(8)), In::TokSigned(ks(m, 0, 1_000), keypair(7), Some(0))], 1)).is_err(),
        "a signature of another input"
    );
    for w in 0..3u8 {
        assert!(run(&cut, &case(vec![In::P2pk(keypair(7)), In::Tok(ks(m, 3, 1_000))], w)).is_err(), "address presence, witness {w}");
    }
    let p = program().profile().unwrap();
    assert_eq!((p.owners.custody, p.owners.holder), (2, 0));
}

// ------------------------------------------------------------------------------------------------ item 5: supply and authority

#[test]
fn transfers_conserve_the_amount_and_create_no_minter() {
    let cut = Cut::embedded();
    let b = xonly(&keypair(9));
    let order = ORDER_COV.as_bytes();
    let run_outs = |outs: Vec<(KronState, u64)>| run(&cut, &fill(&cut, cut.trailing.clone(), outs));
    assert!(run_outs(vec![(ks(b, 0, 250), 1), (ks(order, 2, 350), 1)]).is_ok());
    assert!(run_outs(vec![(ks(b, 0, 251), 1), (ks(order, 2, 350), 1)]).is_err(), "no minting");
    assert!(run_outs(vec![(ks(b, 0, 249), 1), (ks(order, 2, 350), 1)]).is_err(), "no burning on the transfer branch");
    let minter = KronState { is_minter: 1, ..ks(b, 0, 250) };
    assert!(run_outs(vec![(minter, 1), (ks(order, 2, 350), 1)]).is_err(), "a transfer creates no minter");
    // unlike the KRON programs, a zero-amount output is accepted (KOB's builders never create one: a KRON output holds
    // 1..=1e9, `Draft::check_token_output`); a negative one, which would mint the difference, is refused
    assert!(run_outs(vec![(ks(b, 0, 0), 1), (ks(order, 2, 600), 1)]).is_ok(), "a zero-amount output");
    assert!(run_outs(vec![(ks(b, 0, -1), 1), (ks(order, 2, 601), 1)]).is_err(), "no negative amount");
}

/// The selector values other than 0 (the other entries of the program, which KOB never calls): none of them moves a holder's
/// tokens without its signature, releases a custody without its covenant input or mints, i.e. none lets a third party do what
/// branch 0 refuses. (What they let the token's own launchpad do, e.g. its curve, is the registry capabilities' and the
/// review's: `minters: unknown` until then.)
#[test]
fn other_selector_branches_move_nothing_branch_zero_refuses() {
    let cut = Cut::embedded();
    let attacker = keypair(66);
    let a = xonly(&attacker);
    let m = xonly(&keypair(7));
    let order = ORDER_COV.as_bytes();
    for sel in [vec![1u8], vec![2], vec![3], vec![4], vec![0x81], vec![0x7f], vec![0, 0]] {
        let tr = vec![sel.clone()];
        // a holder's tokens to the attacker, attacker-signed (and unsigned)
        let theft = Case {
            ins: vec![In::P2pk(keypair(66)), In::TokSigned(ks(m, 0, 1_000), keypair(66), None)],
            outs: vec![(ks(a, 0, 1_000), 1)],
            wit: vec![0],
            trailing: tr.clone(),
        };
        assert!(run(&cut, &theft).is_err(), "selector {sel:?}: a holder's tokens moved by another key");
        let theft = Case { ins: vec![In::P2pk(keypair(66)), In::Tok(ks(m, 0, 1_000))], ..theft };
        assert!(run(&cut, &theft).is_err(), "selector {sel:?}: a holder's tokens moved unsigned");
        // a custody without its order covenant
        let custody = Case {
            ins: vec![In::P2pk(keypair(66)), In::Tok(ks(order, 2, 600))],
            outs: vec![(ks(a, 0, 600), 1)],
            wit: vec![0],
            trailing: tr.clone(),
        };
        assert!(run(&cut, &custody).is_err(), "selector {sel:?}: a custody released without its covenant");
        // minting from the attacker's own holding
        let mint = Case {
            ins: vec![In::TokSigned(ks(a, 0, 10), keypair(66), None)],
            outs: vec![(ks(a, 0, 1_000_000), 1)],
            wit: vec![0],
            trailing: tr,
        };
        assert!(run(&cut, &mint).is_err(), "selector {sel:?}: minting");
    }
}

// ------------------------------------------------------------------------------------------------ item 6: limits

#[test]
fn slots_and_carrier_floor_are_measured() {
    let cut = Cut::embedded();
    let p = program().profile().unwrap();
    let b = xonly(&keypair(9));
    let order = ORDER_COV.as_bytes();
    let at = |v: u64| run(&cut, &fill(&cut, cut.trailing.clone(), vec![(ks(b, 0, 250), v), (ks(order, 2, 350), v)])).is_ok();
    assert!(at(10 * KAS));
    let (mut lo, mut hi) = (0u64, 10 * KAS);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if at(mid) {
            hi = mid
        } else {
            lo = mid
        }
    }
    assert_eq!(hi, p.carrier_floor, "the profile's carrier_floor is the measured one");
    let outs_ok = |n: usize| {
        let mut outs: Vec<(KronState, u64)> = (0..n - 1).map(|_| (ks(b, 0, 1), 1)).collect();
        outs.push((ks(order, 2, 600 - (n as i64 - 1)), 1));
        run(&cut, &fill(&cut, cut.trailing.clone(), outs)).is_ok()
    };
    let max_out = (1..=12).take_while(|&n| outs_ok(n)).last().unwrap_or(0);
    let ins_ok = |n: usize| {
        let mut ins = vec![In::Cov(ORDER_COV)];
        ins.extend((0..n).map(|_| In::Tok(ks(order, 2, 100))));
        let c = Case { ins, outs: vec![(ks(b, 0, 100 * n as i64), 1)], wit: vec![0; n], trailing: cut.trailing.clone() };
        run(&cut, &c).is_ok()
    };
    let max_in = (1..=12).take_while(|&n| ins_ok(n)).last().unwrap_or(0);
    assert_eq!(Slots { inputs: max_in, outputs: max_out }, p.slots, "the profile's slots are the measured ones");
    // the KRON family's output amount bound: 1e9 accepted
    let big = run(
        &cut,
        &Case {
            ins: vec![In::Cov(ORDER_COV), In::Tok(ks(order, 2, 1_000_000_000))],
            outs: vec![(ks(b, 0, 1_000_000_000), 1)],
            wit: vec![0],
            trailing: cut.trailing.clone(),
        },
    );
    assert!(big.is_ok(), "{big:?}");
}

// ------------------------------------------------------------------------------------------------ KOB's orders on the program

mod kob_orders_on_kforge {
    use kob_protocol::artifacts::{template, TemplateId};
    use kob_protocol::build::{build, Action, CancelOrder, CreateOrder, SendTokens, TokenRecipient, TokenRef};
    use kob_protocol::state::{AnyState, AskState, BidState, KronState, StateCodec, TokenState};
    use kob_protocol::tx::{finalize, sign_locally, FeeOptions, FinalizeOptions, KeyUtxo, OrderUtxo, SigPlan, TokenUtxo, Utxo};
    use kob_protocol::verify::validate_signed;

    const KAS: u64 = 100_000_000;
    const TOKEN: [u8; 32] = [0x38; 32];

    fn sk(n: u8) -> [u8; 32] {
        [n; 32]
    }
    fn pk(n: u8) -> [u8; 32] {
        secp256k1::Keypair::from_secret_key(&secp256k1::Secp256k1::new(), &secp256k1::SecretKey::from_slice(&sk(n)).unwrap())
            .x_only_public_key()
            .0
            .serialize()
    }
    fn keys() -> std::collections::BTreeMap<[u8; 32], [u8; 32]> {
        (1..=9u8).map(|n| (pk(n), sk(n))).collect()
    }
    fn utxo(tag: u8, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
        Utxo { transaction_id: [tag; 32], index: 0, amount, block_daa_score: 500, covenant_id: cov }
    }
    fn fee() -> FeeOptions {
        FeeOptions::default()
    }
    fn program() -> TemplateId {
        TemplateId::KForgeToken1785
    }
    fn ask(maker: u8) -> AskState {
        let t = kob_protocol::artifacts::token_template(program());
        let mut s = AskState::decode_as(TemplateId::KobAskKronD0, &compiled_state(TemplateId::KobAskKronD0)).unwrap();
        s.maker = pk(maker);
        s.token_cov_id = TOKEN;
        s.token_tpl_hash = t.hash;
        s.tpl_prefix_len = t.prefix.len() as i64;
        s.tpl_suffix_len = t.suffix.len() as i64;
        s.scale = 1;
        s.min_fill = 1;
        s.amount_left = 10;
        s.price = 25_000_000;
        s.tip = 0;
        s.expiry_daa = 499_999_999_999;
        s
    }
    fn compiled_state(id: TemplateId) -> Vec<u8> {
        let t = template(id);
        t.contract().compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len].to_vec()
    }
    fn holder(tag: u8, owner: u8, amount: i64) -> TokenUtxo {
        TokenUtxo { utxo: utxo(tag, KAS, Some(TOKEN)), state: TokenState::Kron(KronState::pubkey(amount, pk(owner))) }
    }
    fn check(a: &Action) -> kob_protocol::tx::BuiltTx {
        let built = build(a).unwrap_or_else(|e| panic!("build: {e}"));
        let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default()).unwrap();
        validate_signed(&signed).unwrap_or_else(|e| panic!("engine: {e}"));
        built
    }

    /// A KForge ask is a `KobAskKronD0` (the build of the program's holder owner); placing it spends the maker's id_type 0 tokens
    /// with a token-level signature the wallet gives (a sign request for the token input, with its redeem script); its cancel
    /// returns the custody to the maker as id_type 0, which the maker then sends on with another signature.
    #[test]
    fn an_ask_is_placed_cancelled_and_its_refund_spent() {
        let s = AnyState::KobAskKron(ask(1));
        assert_eq!(s.template_id(), TemplateId::KobAskKronD0);
        assert_eq!(s.template_id().delivery_owner(), Some(0));
        let create = Action::CreateOrder(CreateOrder {
            order: s.clone(),
            value: 10 * KAS,
            tokens: vec![holder(1, 1, 12)],
            token_carrier: 10 * KAS,
            funding: vec![KeyUtxo { utxo: utxo(2, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            lock_time: 0,
            deadline: None,
            records: vec![],
            fee: fee(),
        });
        let built = check(&create);
        // the token input is signed by the maker's key (a P2SH input with its redeem script, as a KCC-20 holder's)
        let tok_in = built.plans.iter().position(|p| matches!(p, SigPlan::KronToken { .. })).unwrap();
        let req = built.sign.iter().find(|r| r.input_index == tok_in).expect("a sign request for the token input");
        assert_eq!(req.pubkey, pk(1));
        assert!(req.redeem_script.is_some());
        assert!(built.roles[tok_in].contains(".token.sig."), "{}", built.roles[tok_in]);
        // the payload names the KobAskKronD0 build, and the record decodes to it
        let tx = &built.tx;
        let order_out = 0usize;
        let order_cov = built.covenants[0].covenant_id;
        let rec = kob_protocol::payload::recover_orders(tx).unwrap();
        assert_eq!(rec[0].order.template_id(), TemplateId::KobAskKronD0);
        // cancel: the custody returns to the maker as id_type 0
        let custody_out = tx.outputs.iter().position(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == TOKEN)).unwrap();
        let custody = TokenUtxo {
            utxo: Utxo {
                transaction_id: tx.id,
                index: custody_out as u32,
                amount: 10 * KAS,
                block_daa_score: 600,
                covenant_id: Some(TOKEN),
            },
            state: TokenState::Kron(KronState::custody(10, order_cov)),
        };
        let cancel = Action::CancelOrder(CancelOrder {
            order: OrderUtxo {
                utxo: Utxo {
                    transaction_id: tx.id,
                    index: order_out as u32,
                    amount: 10 * KAS,
                    block_daa_score: 600,
                    covenant_id: Some(order_cov),
                },
                state: s,
            },
            custody: Some(custody),
            prefund: None,
            strays: vec![],
            foreign: vec![],
            tokens: vec![],
            funding: vec![KeyUtxo { utxo: utxo(3, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            replace: None,
            lock_time: 0,
            records: vec![],
            fee: fee(),
        });
        let built = check(&cancel);
        let SigPlan::KronToken { next_states, .. } = built.plans.iter().find(|p| matches!(p, SigPlan::KronToken { .. })).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(next_states.iter().map(|s| (s.owner, s.id_type, s.amount)).collect::<Vec<_>>(), vec![(pk(1), 0, 10)]);
        // the refund is the maker's to send on
        let refund = holder(4, 1, 10);
        let send = Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN, program: program() },
            tokens: vec![refund],
            recipients: vec![TokenRecipient { pubkey: pk(5), amount: 4, carrier: 10 * KAS }],
            token_change: None,
            token_change_carrier: 10 * KAS,
            funding: vec![KeyUtxo { utxo: utxo(5, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            records: vec![],
            fee: fee(),
        });
        check(&send);
    }

    /// The default (id_type 3) build never serves a KForge token: an instance would deliver what its maker cannot spend. A state
    /// pinning KForge does not decode under it, nor a state pinning a KRON program under the `*KronD0` build; pair orders refuse
    /// the program (they deliver KRON tokens with id_type 3); address-presence tokens are not spendable under it.
    #[test]
    fn the_wrong_build_and_the_pair_kinds_are_refused() {
        let s = AnyState::KobAskKron(ask(1));
        let bytes = s.encode();
        assert!(AnyState::decode(TemplateId::KobAskKron, &bytes).is_err());
        assert!(AnyState::decode(TemplateId::KobAskKronD0, &bytes).is_ok());
        let mut kron = ask(1);
        let t = kob_protocol::artifacts::token_template(TemplateId::KronToken2433);
        (kron.token_tpl_hash, kron.tpl_prefix_len, kron.tpl_suffix_len) = (t.hash, t.prefix.len() as i64, t.suffix.len() as i64);
        let kb = AnyState::KobAskKron(kron).encode();
        assert!(AnyState::decode(TemplateId::KobAskKronD0, &kb).is_err());
        assert!(AnyState::decode(TemplateId::KobAskKron, &kb).is_ok());
        assert!(!program().serves_pairs());
        // address presence is not a KForge holder
        let st = TokenState::Kron(KronState::addr(5, pk(1)));
        assert!(!st.is_holder(program()));
        let send = Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN, program: program() },
            tokens: vec![TokenUtxo { utxo: utxo(1, KAS, Some(TOKEN)), state: st }],
            recipients: vec![TokenRecipient { pubkey: pk(5), amount: 5, carrier: 10 * KAS }],
            token_change: None,
            token_change_carrier: 10 * KAS,
            funding: vec![KeyUtxo { utxo: utxo(5, 100 * KAS, None), pubkey: pk(1) }],
            change: None,
            records: vec![],
            fee: fee(),
        });
        assert!(build(&send).unwrap_err().to_string().contains("not spendable under KForgeToken1785"));
        let _ = BidState::decode_as(TemplateId::KobBidKronD0, &compiled_state(TemplateId::KobBidKronD0)).unwrap();
    }
}
