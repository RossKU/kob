//! Dry run of onboarding a launchpad token program: KForge's token program (testnet-10, template `5b136633...`), a
//! pre-standard program with the KRON 46-byte state behind a one-byte prefix. It is NOT added to the profile table
//! (`crates/kob-protocol/data/programs.json`) and NOT tradable.
//!
//! 1. `kob registry add-program contracts/third-party/kforge/program.bin --name KForgeToken1785 --slots 1/3` derives the
//!    profile from the bytes alone ([`kob_protocol::profile::derive_profile`]): KRON codec, state at offset 1, the
//!    `kron-columns` witness scheme with one trailing push (the prefix `OP_TOALTSTACK` and the `== 0` dispatch after the
//!    state are an entry selector). `contracts/third-party/kforge/profile.candidate.json` is that output plus the facts
//!    measured below.
//! 2. KOB's own `kron-columns` builder with the derived trailing push reproduces the signature script of a real KForge
//!    transfer byte for byte, and the transfer's token outputs are the P2SH of the derived cut (`observed-transfer.json`).
//! 3. The rusty-kaspa engine measures the review facts: custody (id_type 2) released by the order covenant's input is
//!    accepted, refused without the selector push, without that input, or when it mints; 1 token input / 3 outputs per
//!    transaction; a 1-sompi carrier floor; holders are id_type 0 (token-level signature) and address presence (id_type 3)
//!    is REFUSED.
//! 4. That last fact stops the onboarding at the profile check: KOB's KRON order templates deliver to makers with id_type 3
//!    (`TYPE_DELIVERY`, compiled in), which a KForge maker could never spend again. Supporting KForge is therefore not data
//!    alone: it needs KRON order templates built with delivery id_type 0 and a builder for id_type 0 holder spends (a
//!    codec variant, code and review). Everything else about it is data: with holder 3 the candidate is a valid table entry.
//! 5. Its strict-list entry validates as `pending-review`: no token on it can be listed.
//!
//! Still for a reviewer after that (docs/spec/token-programs.md): what the other selector branches let someone do, the
//! genesis rules (KForge's curve "rung" is its own covenant, out of the token's genesis group), the order suites and their
//! ablation on the program, the budget and tip tables.
//!
//! Run: cargo test -p kob-tests --test program_kforge_dry_run -- --nocapture

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

use kob_protocol::profile::{
    derive_profile, registry_template, DeriveOptions, EmbeddedSource, ProfileFile, ProgramInput, ProgramProfile, ProgramReview, Slots,
    SourceKind, WitnessScheme,
};
use kob_protocol::registry::{Family, Registry};
use kob_protocol::script::{kron_columns_sigscript, push_data};
use kob_protocol::state::KronState;

const KFORGE_HASH: &str = "5b136633944a2371a1d0f6a94fdc0d329327ffc700fdec097fc59e76e1d33ae1";
const KAS: u64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x5b; 32]);
const ORDER_COV: Hash = Hash::from_bytes([0xc0; 32]);

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel)
}

fn program_bytes() -> Vec<u8> {
    std::fs::read(repo("contracts/third-party/kforge/program.bin")).expect("program.bin")
}

fn candidate() -> ProgramProfile {
    serde_json::from_str(&std::fs::read_to_string(repo("contracts/third-party/kforge/profile.candidate.json")).unwrap())
        .expect("profile.candidate.json is a profile")
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

/// The program cut by the candidate profile.
struct Cut {
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    trailing: Vec<Vec<u8>>,
}

impl Cut {
    fn of(p: &ProgramProfile) -> Cut {
        let b = program_bytes();
        let l = p.layout;
        Cut { prefix: b[..l.prefix_len].to_vec(), suffix: b[l.prefix_len + l.state_len..].to_vec(), trailing: p.witness.trailing() }
    }
    fn redeem(&self, s: &KronState) -> Vec<u8> {
        [self.prefix.as_slice(), &s.encode(), &self.suffix].concat()
    }
    fn spk(&self, s: &KronState) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

fn ks(owner: [u8; 32], id_type: u8, amount: i64) -> KronState {
    KronState { owner, id_type, amount, is_minter: 0 }
}

#[test]
fn the_profile_derives_from_the_bytes() {
    let bytes = program_bytes();
    let cand = candidate();
    let d = derive_profile(
        &ProgramInput::Redeem(bytes.clone()),
        &DeriveOptions {
            name: Some(cand.name.clone()),
            registry_id: Some(cand.registry_id.clone()),
            source_path: Some(cand.source.path.clone()),
            slots: Some(cand.slots),
            ..Default::default()
        },
    )
    .expect("derivable");
    println!("detection: {}", d.detection);
    for t in &d.review_tasks {
        println!("review task: {t}");
    }
    let p = &d.profile;
    assert_eq!(p.template_hash, KFORGE_HASH);
    assert_eq!((p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len), (1, 46, 1738));
    assert_eq!(p.codec, Family::Kron);
    assert_eq!((p.owners.custody, p.owners.holder), (2, 3));
    assert_eq!(p.witness.scheme, WitnessScheme::KronColumns);
    assert_eq!(p.witness.trailing_pushes, vec![String::new()], "one trailing OP_0: the entry selector of branch 0");
    assert_eq!(p.source.kind, SourceKind::RawRedeem);
    assert!(d.already_embedded.is_none(), "KForge is not embedded in this build");
    assert!(d.review_tasks.iter().any(|t| t.starts_with("entry selector")));
    p.check_shape().expect("the derived profile (owners assumed from the codec family) is a valid entry");
    // the candidate is the derived profile plus the measured facts (owners, slots, floor: see the engine test)
    let byte_facts = |q: &ProgramProfile| (q.template_hash.clone(), q.layout, q.codec, q.witness.clone(), q.source.clone());
    assert_eq!(byte_facts(&cand), byte_facts(p));
    assert_eq!(cand.review.status, ProgramReview::PendingReview);
    assert!(!cand.router_intents);
    // the bytes check the table loader runs
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    cand.check_bytes(&EmbeddedSource::Raw(leaked)).expect("the candidate matches the bytes");
    // the build does not know it: no TemplateId, no token template, no budget
    assert!(kob_protocol::artifacts::TemplateId::from_name(&cand.name).is_none());
    assert!(kob_protocol::artifacts::token_template_by_hash(&cand.hash()).is_none());
}

#[test]
fn the_kron_columns_builder_reproduces_a_real_kforge_transfer() {
    let cand = candidate();
    let cut = Cut::of(&cand);
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
    // the token outputs of that transaction are the P2SH of the derived cut around each next state
    let outs = obs["token_outputs"].as_array().unwrap();
    assert_eq!(outs.len(), next.len());
    for (o, s) in outs.iter().zip(&next) {
        assert_eq!(o["script_public_key"].as_str().unwrap(), kob_protocol::json::to_hex(cut.spk(s).script()), "{s:?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------- engine

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
    /// a KForge token input in this state
    Tok(KronState),
    /// a KForge token input in this state whose signature column carries this key's signature (id_type 0)
    TokSigned(KronState, Keypair),
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
            In::Tok(s) | In::TokSigned(s, _) => UtxoEntry::new(KAS, cut.spk(s), 0, false, Some(TOKEN_COV)),
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
            In::TokSigned(s, kp) => {
                let sig = sign(&unsigned, &entries, n, kp);
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

#[test]
fn the_engine_measures_owners_slots_and_floor() {
    let cand = candidate();
    let cut = Cut::of(&cand);
    let maker = keypair(7);
    let buyer = keypair(9);
    let (m, b) = (xonly(&maker), xonly(&buyer));
    let order = ORDER_COV.as_bytes();

    // custody (id_type 2) released by the order covenant's input: a fill delivering to a buyer, the rest stays in custody
    let fill = |trailing: Vec<Vec<u8>>, outs: Vec<(KronState, u64)>| Case {
        ins: vec![In::Cov(ORDER_COV), In::Tok(ks(order, 2, 600))],
        outs,
        wit: vec![0],
        trailing,
    };
    let honest = vec![(ks(b, 0, 250), 1), (ks(order, 2, 350), 1)];
    let r = run(&cut, &fill(cut.trailing.clone(), honest.clone()));
    println!("KForge custody released by the order covenant: {r:?}");
    assert!(r.is_ok(), "{r:?}");
    assert!(run(&cut, &fill(vec![], honest.clone())).is_err(), "the selector push is required");
    assert!(run(&cut, &fill(cut.trailing.clone(), vec![(ks(b, 0, 251), 1), (ks(order, 2, 350), 1)])).is_err(), "no minting");
    let stolen = Case { ins: vec![In::P2pk(buyer), In::Tok(ks(order, 2, 600))], ..fill(cut.trailing.clone(), honest.clone()) };
    assert!(run(&cut, &stolen).is_err(), "custody without the order covenant's input is refused");

    // holders: KForge authorises id_type 0 (a token-level signature in the signature column) and refuses id_type 3
    // (address presence: a P2PK input of the owner), whatever the witness byte. KOB's KRON order templates deliver to makers
    // with id_type 3 (TYPE_DELIVERY, compiled in), so a KForge maker's delivery could never be spent again.
    let outs = vec![(ks(order, 2, 600), 1), (ks(m, 0, 400), 1)];
    let r0 = run(
        &cut,
        &Case { ins: vec![In::TokSigned(ks(m, 0, 1_000), maker)], outs: outs.clone(), wit: vec![0], trailing: cut.trailing.clone() },
    );
    println!("KForge holder id_type 0 (token-level signature): {r0:?}");
    assert!(r0.is_ok(), "{r0:?}");
    for w in 0..3u8 {
        let r3 = run(
            &cut,
            &Case {
                ins: vec![In::P2pk(maker), In::Tok(ks(m, 3, 1_000))],
                outs: outs.clone(),
                wit: vec![w],
                trailing: cut.trailing.clone(),
            },
        );
        println!("KForge holder id_type 3 (address presence), witness {w}: {r3:?}");
        assert!(r3.is_err(), "address presence is not an owner type of KForge");
    }
    assert_eq!((cand.owners.custody, cand.owners.holder), (2, 0), "the candidate records the measured owner types");

    // carrier floor: the smallest token output value accepted
    let at = |v: u64| run(&cut, &fill(cut.trailing.clone(), vec![(ks(b, 0, 250), v), (ks(order, 2, 350), v)])).is_ok();
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
    println!("KForge carrier floor (measured): {hi} sompi");
    assert_eq!(hi, cand.carrier_floor, "the candidate's carrier_floor is the measured one");

    // slots: the largest token output and input counts one transfer accepts
    let outs_ok = |n: usize| {
        let mut outs: Vec<(KronState, u64)> = (0..n - 1).map(|_| (ks(b, 0, 1), 1)).collect();
        outs.push((ks(order, 2, 600 - (n as i64 - 1)), 1));
        run(&cut, &fill(cut.trailing.clone(), outs)).is_ok()
    };
    let max_out = (1..=12).take_while(|&n| outs_ok(n)).last().unwrap_or(0);
    let ins_ok = |n: usize| {
        let mut ins = vec![In::Cov(ORDER_COV)];
        ins.extend((0..n).map(|_| In::Tok(ks(order, 2, 100))));
        let c = Case { ins, outs: vec![(ks(b, 0, 100 * n as i64), 1)], wit: vec![0; n], trailing: cut.trailing.clone() };
        run(&cut, &c).is_ok()
    };
    let max_in = (1..=12).take_while(|&n| ins_ok(n)).last().unwrap_or(0);
    println!("KForge slots (measured): {max_in} inputs / {max_out} outputs");
    assert_eq!(Slots { inputs: max_in, outputs: max_out }, cand.slots, "the candidate's slots are the measured ones");
}

/// The measured holder owner (id_type 0) is not the one KOB's KRON order templates deliver to (3): the profile validator
/// refuses the candidate with that reason, so KForge cannot be added as data alone; it needs KRON order templates built
/// with `TYPE_DELIVERY = 0` and a builder for id_type 0 holder spends (a new codec variant: code and review).
#[test]
fn the_candidate_is_refused_until_kob_delivers_with_its_holder_owner() {
    let cand = candidate();
    let e = cand.check_shape().unwrap_err();
    println!("profile check: {e}");
    assert!(e.message.contains("the kron order templates use custody 2 and holder 3"), "{e}");
    let mut table = ProfileFile::parse(kob_protocol::profile::PROGRAMS_JSON).unwrap();
    table.programs.push(cand.clone());
    assert!(table.check().is_err(), "the table refuses it");
    // with the holder owner the KRON templates use, everything else about the candidate is a valid table entry
    let mut as_kron = cand.clone();
    as_kron.owners.holder = 3;
    let mut table = ProfileFile::parse(kob_protocol::profile::PROGRAMS_JSON).unwrap();
    table.programs.push(as_kron);
    table.check().expect("only the owner type stands between the candidate and the table");
}

#[test]
fn its_strict_list_entry_is_pending_review_and_lists_nothing() {
    let cand = candidate();
    let mut reg = Registry::default_registry();
    let entry =
        serde_json::to_value(registry_template(&cand, Some("KForge launchpad (api-test.kforge.xyz), testnet-10; observed on chain")))
            .unwrap();
    let mut v = serde_json::to_value(&reg).unwrap();
    v["templates"].as_array_mut().unwrap().push(entry);
    reg = Registry::parse(&v.to_string()).expect("the proposed entry validates");
    let t = reg.template(&cand.registry_id).unwrap();
    assert!(!t.is_strict(), "pending-review: not on the strict list");
    assert!(reg.strict_template_by_hash(&cand.hash()).is_none());
    // a token on it cannot be listed
    v = serde_json::to_value(&reg).unwrap();
    v["tokens"].as_array_mut().unwrap().push(serde_json::json!({
        "ticker": "KFTEST", "name": "KForge test", "family": "kron", "covenant_id": "38af6574f72d330f506eb6690e8589a0a71771cb651ed9c5cd63b3c92a923d94",
        "template_id": cand.registry_id, "extension_commitment": null, "extension_class": "none", "decimals": 0,
        "status": "listed", "verified": true,
    }));
    let e = Registry::parse(&v.to_string()).unwrap_err().to_string();
    assert!(e.contains("review"), "{e}");
}
