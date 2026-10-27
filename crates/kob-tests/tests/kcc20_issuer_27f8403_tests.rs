//! The issuers of argent-lang/kcc20-reference `27f8403` (`PublicMint`, `TokenSeed`: the `c8a0871` issuers plus the owner-signed
//! entry `rebalance_value`), next to the `c8a0871` ones the genesis check has accepted since 2026-10-13. Real fixtures,
//! executed in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context: the programs are the vendored upstream
//! artifacts (`contracts/third-party/kcc20-reference/`, `kob_protocol::public_mint`), the holders KOB's pinned handle.
//!
//!   - a genesis built with either build's issuers verifies and reports which build it is (`GenesisReport::issuer`, the
//!     registry record's `issuer`); holders only have none; the registry record validates;
//!   - tampered genesis fail: a changed issuer program, a foreign or the other build's template field, a mix of the two builds,
//!     the issuers under another program;
//!   - what `rebalance_value` can and cannot do, in the engine: with the owner's signature it recreates the very same cell (the
//!     state and so the allowance are untouched) at any value; it is refused without that signature, for another transaction, with
//!     a changed state or script, with a second or an unbound output, with a holder created beside it and with another input of
//!     the covenant id; the `c8a0871` issuers have no such entry; the successor still mints or creates like the original;
//!   - the indexer's reading of issuer calls (`created_holder`) works for the new issuers and sees no holder in a rebalance.
//!
//! Run: cargo test --release -p kob-tests --test kcc20_issuer_27f8403_tests -- --nocapture

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint,
    TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::opcodes::codes::OpTrue;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::genesis_evidence::{verify_token, TokenEvidence};
use kob_protocol::issue::TokenState;
use kob_protocol::public_mint::{
    created_holder, decode_companion_versioned, mint_sigscript_of, minter_redeem_of, public_mint_of, seed_redeem_of, token_seed_of,
    Companion, IssuerVersion, MinterState, SeedState, KCC20_SIL_HASH,
};
use kob_protocol::registry::{verify_genesis_of, GenesisError, GenesisOutput, GenesisReport, Registry};
use kob_protocol::script::{p2sh_spk, push_data};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde_json::{json, Value};
use silverscript_abi::{encode_contract_entry_sig_script, ArtifactValue, SilAbiArtifact};

use IssuerVersion::{C8a0871, F27f8403};

const PM: TemplateId = TemplateId::Kcc20PublicMint;
const KAS: u64 = 100_000_000;
const EXT: [u8; 32] = [0x5e; 32];
const OWNER_COV: [u8; 32] = [0xc0; 32];
const FUNDING: ([u8; 32], u32) = ([0x99; 32], 1);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn abi(v: IssuerVersion) -> SilAbiArtifact {
    let text = match v {
        C8a0871 => include_str!("../../../contracts/third-party/kcc20-reference/public-mint.artifact.json"),
        F27f8403 => include_str!("../../../contracts/third-party/kcc20-reference/public-mint.27f8403.artifact.json"),
    };
    let doc: Value = serde_json::from_str(text).unwrap();
    serde_json::from_value(doc["sil_abi"].clone()).unwrap()
}

/// The issuers' deposit owner: a real key, so that `rebalance_value` can be signed.
fn owner() -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[0x42; 32]).unwrap())
}
fn owner_pk() -> [u8; 32] {
    owner().x_only_public_key().0.serialize()
}
fn stranger() -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&[0x43; 32]).unwrap())
}

fn holder(amount: u64, ext: [u8; 32]) -> TokenState {
    TokenState { amount, owner: OWNER_COV, owner_scheme: 4, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment: ext }
}
fn holder_redeem(s: &TokenState) -> Vec<u8> {
    token_template(PM).redeem(&s.encode())
}
fn minter(remaining: i64, mint_amount: i64) -> MinterState {
    MinterState { remaining, mint_amount, owner: owner_pk(), extension_commitment: EXT }
}
fn seed() -> SeedState {
    SeedState { owner: owner_pk(), extension_commitment: EXT }
}
fn kcc20_sil_cut() -> (Vec<u8>, Vec<u8>) {
    let h = token_template(PM);
    (h.prefix[..h.prefix.len() - 33].to_vec(), h.suffix.clone())
}

struct Genesis {
    tx: Transaction,
    redeems: Vec<Vec<u8>>,
    cov: Hash,
}

fn exec(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    kob_protocol::verify::execute(tx, entries, false).expect("tx-level")
}

fn genesis(redeems: Vec<Vec<u8>>) -> Genesis {
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(FUNDING.0), index: FUNDING.1 };
    let mut outputs: Vec<TransactionOutput> =
        redeems.iter().map(|r| TransactionOutput { value: 10 * KAS, script_public_key: p2sh_spk(r), covenant: None }).collect();
    let cov = covenant_id(op, outputs.iter().enumerate().map(|(i, o)| (i as u32, o)));
    for o in outputs.iter_mut() {
        o.covenant = Some(CovenantBinding { authorizing_input: 0, covenant_id: cov });
    }
    let input = TransactionInput::new_with_compute_budget(op, vec![], 0, 0);
    let tx = Transaction::new(1, vec![input], outputs, 0, Default::default(), 0, vec![]);
    let entries = vec![UtxoEntry::new(100 * KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None)];
    let res = exec(&tx, &entries);
    assert!(res.iter().all(|r| r.is_ok()), "genesis covenant context: {res:?}");
    Genesis { tx, redeems, cov }
}

impl Genesis {
    fn outputs(&self) -> Vec<GenesisOutput> {
        self.tx
            .outputs
            .iter()
            .zip(&self.redeems)
            .enumerate()
            .map(|(i, (o, r))| GenesisOutput {
                index: i as u32,
                value: o.value,
                script_public_key: o.script_public_key.clone(),
                redeem_script: Some(r.clone()),
            })
            .collect()
    }
    fn check(&self) -> Result<GenesisReport, GenesisError> {
        self.check_as("kcc20-ref-public-mint")
    }
    fn check_as(&self, template_id: &str) -> Result<GenesisReport, GenesisError> {
        let r = Registry::default_registry();
        let tpl = r.template(template_id).unwrap();
        verify_genesis_of(tpl, &hex(&self.cov.as_bytes()), &hex(&self.tx.id().as_bytes()), FUNDING, &self.outputs(), Some(EXT))
    }
    fn outpoint(&self, i: u32) -> TransactionOutpoint {
        TransactionOutpoint { transaction_id: self.tx.id(), index: i }
    }
    fn entry(&self, i: usize) -> UtxoEntry {
        UtxoEntry::new(self.tx.outputs[i].value, self.tx.outputs[i].script_public_key.clone(), 0, false, Some(self.cov))
    }
    fn bind(&self) -> Option<CovenantBinding> {
        Some(CovenantBinding { authorizing_input: 0, covenant_id: self.cov })
    }
}

/// Upstream's recommended genesis of build `v`: one minter of the advertised supply, one seed.
fn recommended(v: IssuerVersion) -> Genesis {
    genesis(vec![minter_redeem_of(v, &minter(21_000_000, 1_000)).unwrap(), seed_redeem_of(v, &seed())])
}

// ---------------------------------------------------------------- the genesis check

#[test]
fn a_genesis_of_either_issuer_build_verifies_and_reports_which_build_it_is() {
    for v in IssuerVersion::ALL {
        let r = recommended(v).check().unwrap_or_else(|e| panic!("{}: {e}", v.label()));
        assert_eq!((r.supply, r.mint_allowance, r.minter_outputs.clone(), r.seed_outputs.clone()), (0, 21_000_000, vec![0], vec![1]));
        assert_eq!(r.issuer.as_deref(), Some(v.label()));
        assert_eq!(r.genesis_supply().max_supply(), Some(21_000_000));

        // pre-minted holders, two minters and two seeds
        let g = genesis(vec![
            holder_redeem(&holder(500, EXT)),
            minter_redeem_of(v, &minter(1_000, 10)).unwrap(),
            seed_redeem_of(v, &seed()),
            minter_redeem_of(v, &minter(0, 10)).unwrap(),
            seed_redeem_of(v, &SeedState { owner: [0x01; 32], extension_commitment: EXT }),
            holder_redeem(&holder(250, EXT)),
        ]);
        let r = g.check().unwrap();
        assert_eq!((r.supply, r.mint_allowance, r.issuer.as_deref()), (750, 1_000, Some(v.label())));
        assert_eq!((r.minter_outputs.clone(), r.seed_outputs.clone()), (vec![1, 3], vec![2, 4]));

        // a seed alone (no minter) is an issuer too: fixed supply, but the build is still reported
        let g = genesis(vec![holder_redeem(&holder(1_000, EXT)), seed_redeem_of(v, &seed())]);
        let r = g.check().unwrap();
        assert_eq!((r.supply, r.mint_allowance, r.issuer.as_deref()), (1_000, 0, Some(v.label())));
    }
    // holders only: no issuer, whichever build the token could have used
    let g = genesis(vec![holder_redeem(&holder(1_000, EXT))]);
    let r = g.check().unwrap();
    assert_eq!((r.supply, r.mint_allowance, r.issuer), (1_000, 0, None));
}

fn evidence_of(g: &Genesis) -> TokenEvidence {
    let outputs: Vec<Value> = g
        .tx
        .outputs
        .iter()
        .map(|o| {
            json!({"value": o.value, "spk_version": o.script_public_key.version(), "spk": hex(o.script_public_key.script()),
                "covenant": o.covenant.map(|c| json!({"authorizing_input": c.authorizing_input, "covenant_id": hex(&c.covenant_id.as_bytes())}))})
        })
        .collect();
    let reveals: Vec<Value> =
        g.redeems.iter().enumerate().map(|(i, r)| json!({"index": i, "redeem_script": hex(r), "source": "test"})).collect();
    serde_json::from_value(json!({
        "ticker": "PMT",
        "covenant_id": hex(&g.cov.as_bytes()),
        "template_id": "kcc20-ref-public-mint",
        "genesis": {
            "txid": hex(&g.tx.id().as_bytes()),
            "accepting_block_daa_score": 1_000,
            "source": "test fixture",
            "tx": {
                "version": g.tx.version,
                "inputs": [{"txid": hex(&FUNDING.0), "index": FUNDING.1, "sequence": 0}],
                "outputs": outputs,
                "lock_time": 0,
                "subnetwork_id": "00".repeat(20),
                "gas": 0,
                "payload": "",
            },
            "reveals": reveals,
        },
        "live_minters": Value::Null,
    }))
    .unwrap()
}

fn registry_with(g: &Genesis) -> Registry {
    let mut r = Registry::default_registry();
    r.tokens.push(
        serde_json::from_value(json!({
            "ticker": "PMT", "name": "Public mint test", "family": "kcc20", "covenant_id": hex(&g.cov.as_bytes()),
            "template_id": "kcc20-ref-public-mint", "extension_commitment": hex(&EXT), "extension_class": "other",
            "decimals": 0, "verified": false,
        }))
        .unwrap(),
    );
    r
}

/// The registry record (`kob registry verify-genesis` over evidence) names the build; it validates, round-trips, and the
/// validator refuses a build KOB does not pin.
#[test]
fn the_registry_record_names_the_issuer_build() {
    for v in IssuerVersion::ALL {
        let g = recommended(v);
        let reg = registry_with(&g);
        let rec = verify_token(&reg, &evidence_of(&g), 2_000).result.expect("verified");
        assert_eq!(rec.issuer.as_deref(), Some(v.label()));
        assert_eq!((rec.supply, rec.mint_allowance), (0, Some(21_000_000)));
        let back: kob_protocol::registry::GenesisRecord = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert_eq!(back, rec);
        let mut reg2 = reg.clone();
        let t = reg2.tokens.last_mut().unwrap();
        t.verified = true;
        t.genesis_verified = Some(true);
        t.genesis = Some(rec);
        reg2.validate().expect("a record that names a pinned build");
        reg2.tokens.last_mut().unwrap().genesis.as_mut().unwrap().issuer = Some("kcc20-reference@0000000".into());
        assert!(reg2.validate().is_err(), "an unpinned build is refused");
    }
    // holders only: no issuer key in the record
    let fixed = genesis(vec![holder_redeem(&holder(1_000, EXT))]);
    let rec = verify_token(&registry_with(&fixed), &evidence_of(&fixed), 2_000).result.unwrap();
    assert_eq!(rec.issuer, None);
    assert!(serde_json::to_value(&rec).unwrap().get("issuer").is_none());
    // the two builds give two distinct records of the same facts
    let (a, b) = (recommended(C8a0871), recommended(F27f8403));
    let ra = verify_token(&registry_with(&a), &evidence_of(&a), 2_000).result.unwrap();
    let rb = verify_token(&registry_with(&b), &evidence_of(&b), 2_000).result.unwrap();
    assert_ne!(ra.issuer, rb.issuer);
}

#[test]
fn tampered_issuer_genesis_fail() {
    for v in IssuerVersion::ALL {
        let other = IssuerVersion::ALL.into_iter().find(|o| *o != v).unwrap();
        let good_m = minter_redeem_of(v, &minter(1_000, 10)).unwrap();
        let good_s = seed_redeem_of(v, &seed());

        // a changed byte in the program (first, middle, last of the suffix): an instance of nothing
        for at in [good_m.len() - 1, good_m.len() - 700, 1 + 150 + 1] {
            let mut bad = good_m.clone();
            bad[at] ^= 1;
            let g = genesis(vec![bad, good_s.clone()]);
            assert_eq!(g.check(), Err(GenesisError::NotTemplate(0, "kcc20-ref-public-mint".into())), "{} byte {at}", v.label());
        }
        let mut bad = good_s.clone();
        let n = bad.len();
        bad[n - 300] ^= 1;
        assert_eq!(
            genesis(vec![good_m.clone(), bad]).check(),
            Err(GenesisError::NotTemplate(1, "kcc20-ref-public-mint".into())),
            "{} seed",
            v.label()
        );

        // the other build's own-template field inside this build's program, and a foreign KCC20 template
        let mut st = kob_protocol::public_mint::minter_state_bytes_of(v, &minter(1_000, 10)).unwrap();
        st[34..66].copy_from_slice(&other_hash(other, true));
        let g = genesis(vec![public_mint_of(v).redeem(&st), good_s.clone()]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(0, ref e)) if e.contains("template")), "{:?}", g.check());
        let mut st = kob_protocol::public_mint::seed_state_bytes_of(v, &seed());
        st[34..66].copy_from_slice(&other_hash(other, false));
        let g = genesis(vec![good_m.clone(), token_seed_of(v).redeem(&st)]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(1, ref e)) if e.contains("template")), "{:?}", g.check());
        let mut st = kob_protocol::public_mint::minter_state_bytes_of(v, &minter(1_000, 10)).unwrap();
        st[1..33].copy_from_slice(&[0x42; 32]);
        let g = genesis(vec![public_mint_of(v).redeem(&st), good_s.clone()]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(0, ref e)) if e.contains("gen__kcc20_template")));

        // a minter that cannot mint, a second extension commitment, a hidden script beside the issuers
        let g = genesis(vec![minter_redeem_of(v, &minter(1_000, 0)).unwrap(), good_s.clone()]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(0, ref e)) if e.contains("not a minter")));
        let g = genesis(vec![good_m.clone(), seed_redeem_of(v, &SeedState { owner: owner_pk(), extension_commitment: [1; 32] })]);
        assert!(matches!(g.check(), Err(GenesisError::ExtensionCommitment(1, ..))));
        let g = genesis(vec![good_m.clone(), good_s.clone(), vec![OpTrue]]);
        assert_eq!(g.check(), Err(GenesisError::NotTemplate(2, "kcc20-ref-public-mint".into())));

        // the issuers in the genesis of another program are foreign outputs there
        let g = genesis(vec![token_template(TemplateId::Kcc20Ref).redeem(&holder(5, EXT).encode()), good_m.clone()]);
        assert_eq!(g.check_as("kcc20-ref-3x3"), Err(GenesisError::NotTemplate(1, "kcc20-ref-3x3".into())));
    }
    // a genesis never mixes the two builds' issuers
    for (a, b) in [(C8a0871, F27f8403), (F27f8403, C8a0871)] {
        let g = genesis(vec![minter_redeem_of(a, &minter(1_000, 10)).unwrap(), seed_redeem_of(b, &seed())]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(1, ref e)) if e.contains("mixes issuer builds")), "{:?}", g.check());
        let g = genesis(vec![minter_redeem_of(a, &minter(1_000, 10)).unwrap(), minter_redeem_of(b, &minter(5, 1)).unwrap()]);
        assert!(g.check().is_err());
    }
}

fn other_hash(v: IssuerVersion, minter: bool) -> [u8; 32] {
    let h = if minter { v.public_mint_hash() } else { v.token_seed_hash() };
    (0..32).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>().try_into().unwrap()
}

// ---------------------------------------------------------------- issuer calls

fn kcc20_state_value(s: &TokenState) -> ArtifactValue {
    s.to_abi()
}

/// `PublicMint.mint` (or `TokenSeed.create`) of the genesis issuer at output `at`, build `v`.
fn issue_tx(
    v: IssuerVersion,
    g: &Genesis,
    at: usize,
    recipient: &TokenState,
    next_issuer: &[u8],
    minter_call: bool,
) -> (Transaction, Vec<UtxoEntry>) {
    let (p, s) = kcc20_sil_cut();
    let (contract, entry) = if minter_call { ("PublicMint", "mint") } else { ("TokenSeed", "create") };
    let mut ss =
        encode_contract_entry_sig_script(&abi(v), contract, entry, &[kcc20_state_value(recipient), p.into(), s.into()]).unwrap();
    ss.extend(push_data(&g.redeems[at]));
    let mut inputs = vec![TransactionInput::new_with_compute_budget(g.outpoint(at as u32), ss, 0, 0)];
    let mut entries = vec![g.entry(at)];
    fee_input(&mut inputs, &mut entries);
    let outputs = vec![
        TransactionOutput { value: g.tx.outputs[at].value, script_public_key: p2sh_spk(next_issuer), covenant: g.bind() },
        TransactionOutput { value: KAS, script_public_key: p2sh_spk(&holder_redeem(recipient)), covenant: g.bind() },
    ];
    (Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]), entries)
}

fn fee_input(inputs: &mut Vec<TransactionInput>, entries: &mut Vec<UtxoEntry>) {
    inputs.push(TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xfe; 32]), index: 0 },
        vec![],
        0,
        0,
    ));
    entries.push(UtxoEntry::new(50 * KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None));
}

#[test]
fn the_new_issuers_still_mint_and_create_holders_of_the_pinned_template_and_the_indexer_reads_the_calls() {
    let g = recommended(F27f8403);
    let minted = holder(1_000, EXT);
    let next = minter_redeem_of(F27f8403, &minter(21_000_000 - 1_000, 1_000)).unwrap();
    let (tx, e) = issue_tx(F27f8403, &g, 0, &minted, &next, true);
    let res = exec(&tx, &e);
    assert!(res.iter().all(|r| r.is_ok()), "mint: {res:?}");
    assert_eq!(token_template(PM).state_of(&holder_redeem(&minted)), Some(minted.encode().as_slice()));
    // above the per-mint limit, an allowance that is not reduced
    let big = holder(1_001, EXT);
    let (tx2, e2) = issue_tx(F27f8403, &g, 0, &big, &minter_redeem_of(F27f8403, &minter(21_000_000 - 1_001, 1_000)).unwrap(), true);
    assert!(exec(&tx2, &e2)[0].is_err());
    let (tx3, e3) = issue_tx(F27f8403, &g, 0, &minted, &minter_redeem_of(F27f8403, &minter(21_000_000, 1_000)).unwrap(), true);
    assert!(exec(&tx3, &e3)[0].is_err());
    // a seed creates a zero-amount holder and no amount
    let (tx4, e4) = issue_tx(F27f8403, &g, 1, &holder(0, EXT), &g.redeems[1], false);
    assert!(exec(&tx4, &e4).iter().all(|r| r.is_ok()));
    let (tx5, e5) = issue_tx(F27f8403, &g, 1, &holder(1, EXT), &g.redeems[1], false);
    assert!(exec(&tx5, &e5)[0].is_err());

    // the indexer's reading of the calls: the holder each creates, with the holder program
    let m = minter(21_000_000, 1_000);
    let st = kob_protocol::state::Kcc20State::p2pk(250, [3; 32], EXT);
    for v in IssuerVersion::ALL {
        assert_eq!(created_holder(&mint_sigscript_of(v, &m, &st).unwrap()), Some((PM, st.clone())), "{}", v.label());
    }
    assert_eq!(
        created_holder(&tx.inputs[0].signature_script),
        Some((PM, kob_protocol::state::Kcc20State::custody(1_000, OWNER_COV, EXT)))
    );
}

// ---------------------------------------------------------------- rebalance_value

fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(1).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(1);
    s
}

/// How a `rebalance_value` transaction is varied.
#[derive(Clone)]
struct Rebalance {
    /// the output's script (None: the input's own, the identical successor)
    script: Option<Vec<u8>>,
    /// the output's value
    value: u64,
    /// the output is bound to the covenant id
    bound: bool,
    /// outputs beside the successor, bound to the covenant id (redeem scripts)
    extra_bound: Vec<Vec<u8>>,
    /// a second input of the covenant id beside the issuer
    other_cov_input: bool,
    /// who signs
    signer: Option<Keypair>,
    /// the entry is called under a different build's ABI (for the build that has no such entry)
    abi_of: IssuerVersion,
}

fn rb(v: IssuerVersion) -> Rebalance {
    Rebalance {
        script: None,
        value: 3 * KAS,
        bound: true,
        extra_bound: vec![],
        other_cov_input: false,
        signer: Some(owner()),
        abi_of: v,
    }
}

/// A `rebalance_value` spend of the genesis issuer at output `at` (contract `contract`, build `v`).
fn rebalance_tx(g: &Genesis, at: usize, contract: &str, r: &Rebalance) -> (Transaction, Vec<UtxoEntry>) {
    let a = abi(r.abi_of);
    let call = |sig: &[u8]| {
        let mut ss = encode_contract_entry_sig_script(&a, contract, "rebalance_value", &[ArtifactValue::from(sig.to_vec())]).unwrap();
        ss.extend(push_data(&g.redeems[at]));
        ss
    };
    let mut inputs = vec![TransactionInput::new_with_compute_budget(g.outpoint(at as u32), call(&[0u8; 65]), 0, 0)];
    let mut entries = vec![g.entry(at)];
    if r.other_cov_input {
        // a holder of the token's covenant family beside the issuer
        inputs.push(TransactionInput::new_with_compute_budget(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xaa; 32]), index: 0 },
            vec![],
            0,
            0,
        ));
        entries.push(UtxoEntry::new(KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(g.cov)));
    }
    fee_input(&mut inputs, &mut entries);
    let spk = match &r.script {
        Some(s) => p2sh_spk(s),
        None => g.tx.outputs[at].script_public_key.clone(),
    };
    let mut outputs =
        vec![TransactionOutput { value: r.value, script_public_key: spk, covenant: if r.bound { g.bind() } else { None } }];
    for extra in &r.extra_bound {
        outputs.push(TransactionOutput { value: KAS, script_public_key: p2sh_spk(extra), covenant: g.bind() });
    }
    let mut tx = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let sig = match &r.signer {
        Some(kp) => sign(&tx, &entries, 0, kp),
        None => vec![0u8; 65],
    };
    tx.inputs[0].signature_script = call(&sig);
    (tx, entries)
}

fn ok(res: &[Result<u64, String>]) -> bool {
    res.iter().all(|r| r.is_ok())
}

/// What the owner can do with `rebalance_value` on the `27f8403` issuers, and what it cannot.
#[test]
fn rebalance_value_changes_only_the_deposit_and_only_with_the_owner() {
    let g = recommended(F27f8403);
    for (at, contract) in [(0usize, "PublicMint"), (1, "TokenSeed")] {
        let base = || rb(F27f8403);
        // accepted: the identical successor at a lower and at a higher value
        for value in [KAS / 10, 3 * KAS, 40 * KAS] {
            let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { value, ..base() });
            let res = exec(&tx, &e);
            assert!(ok(&res), "{contract} at {value}: {res:?}");
            // the successor is the same cell: the same script public key, hence the same state and allowance
            assert_eq!(tx.outputs[0].script_public_key, g.tx.outputs[at].script_public_key);
            let (v, c) = decode_companion_versioned(&g.redeems[at]).unwrap().unwrap();
            assert_eq!(v, F27f8403);
            if let Companion::Minter(m) = c {
                assert_eq!(m.remaining, 21_000_000);
            }
        }

        // not the owner: another key, no signature, the owner's signature of another transaction
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { signer: Some(stranger()), ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: another key");
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { signer: None, ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: no signature");
        let (tx_a, e_a) = rebalance_tx(&g, at, contract, &base());
        let (mut tx_b, e_b) = rebalance_tx(&g, at, contract, &Rebalance { value: 7 * KAS, ..base() });
        tx_b.inputs[0].signature_script = tx_a.inputs[0].signature_script.clone();
        assert!(ok(&exec(&tx_a, &e_a)));
        assert!(exec(&tx_b, &e_b)[0].is_err(), "{contract}: a signature does not carry to another transaction");

        // the successor must be the same script: another owner, or (minter) another allowance
        let mut other_owner = g.redeems[at].clone();
        let pos = other_owner.windows(32).position(|w| w == owner_pk()).expect("owner key in the state");
        other_owner[pos] ^= 1;
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(other_owner), ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: another owner");
        if contract == "PublicMint" {
            let less = minter_redeem_of(F27f8403, &minter(21_000_000 - 1, 1_000)).unwrap();
            let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(less), ..base() });
            assert!(exec(&tx, &e)[0].is_err(), "{contract}: no allowance change");
            let more = minter_redeem_of(F27f8403, &minter(21_000_001, 1_000)).unwrap();
            let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(more), ..base() });
            assert!(exec(&tx, &e)[0].is_err(), "{contract}: no allowance increase");
            let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(g.redeems[1].clone()), ..base() });
            assert!(exec(&tx, &e)[0].is_err(), "{contract}: not turned into the other issuer");
        } else {
            let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(g.redeems[0].clone()), ..base() });
            assert!(exec(&tx, &e)[0].is_err(), "{contract}: a seed does not become a minter");
        }
        // the old build's issuer is not what the successor may be either
        let old = if contract == "PublicMint" {
            minter_redeem_of(C8a0871, &minter(21_000_000, 1_000)).unwrap()
        } else {
            seed_redeem_of(C8a0871, &seed())
        };
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { script: Some(old), ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: not the other build");

        // exactly one successor: an unbound output, no output beside it, a holder or a second issuer created beside it
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { bound: false, ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: successor not bound to the covenant");
        let new_holder = holder_redeem(&holder(1_000_000, EXT));
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { extra_bound: vec![new_holder], ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: no holder is created");
        let second = g.redeems[at].clone();
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { extra_bound: vec![second], ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: no second issuer is created");
        // no other input of the covenant id beside the issuer
        let (tx, e) = rebalance_tx(&g, at, contract, &Rebalance { other_cov_input: true, ..base() });
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: a lone covenant input");
    }
}

/// The `c8a0871` issuers have no `rebalance_value`: the same call against them is refused (their deposit cannot be changed).
#[test]
fn the_c8a0871_issuers_have_no_rebalance_entry() {
    let g = recommended(C8a0871);
    for (at, contract) in [(0usize, "PublicMint"), (1, "TokenSeed")] {
        // encode the call under the 27f8403 ABI (the old ABI has no such entry) against the old program
        let r = Rebalance { abi_of: F27f8403, ..rb(C8a0871) };
        let (tx, e) = rebalance_tx(&g, at, contract, &r);
        assert!(exec(&tx, &e)[0].is_err(), "{contract}");
    }
    assert!(abi(C8a0871).contracts["PublicMint"].entry("rebalance_value").is_none());
    assert!(abi(F27f8403).contracts["PublicMint"].entry("rebalance_value").is_some());
    assert!(abi(F27f8403).contracts["TokenSeed"].entry("rebalance_value").is_some());
}

/// After a rebalance the issuer is the same cell at another value: it mints (or creates) as before, keeps its new deposit, and
/// the indexer sees no holder in the rebalance itself.
#[test]
fn a_rebalanced_issuer_goes_on_minting_with_its_new_deposit() {
    let g = recommended(F27f8403);
    for (at, contract, minter_call) in [(0usize, "PublicMint", true), (1, "TokenSeed", false)] {
        let (rtx, re) = rebalance_tx(&g, at, contract, &Rebalance { value: 4 * KAS, ..rb(F27f8403) });
        assert!(ok(&exec(&rtx, &re)));
        assert_eq!(created_holder(&rtx.inputs[0].signature_script), None, "{contract}: a rebalance creates no holder");

        // the successor cell (output 0 of the rebalance) as a genesis-like source of the next call
        let succ = Genesis {
            tx: Transaction::new(1, vec![], rtx.outputs.clone(), 0, Default::default(), 0, vec![]),
            redeems: vec![g.redeems[at].clone()],
            cov: g.cov,
        };
        let recipient = if minter_call { holder(500, EXT) } else { holder(0, EXT) };
        let next =
            if minter_call { minter_redeem_of(F27f8403, &minter(21_000_000 - 500, 1_000)).unwrap() } else { g.redeems[at].clone() };
        let (tx, e) = issue_tx_from(&succ, &rtx, &recipient, &next, minter_call, 4 * KAS);
        let res = exec(&tx, &e);
        assert!(ok(&res), "{contract} from the rebalanced cell: {res:?}");
        // the deposit is preserved by every issuer call (`value == self.value`): a call that moves it is refused
        let (tx, e) = issue_tx_from(&succ, &rtx, &recipient, &next, minter_call, 5 * KAS);
        assert!(exec(&tx, &e)[0].is_err(), "{contract}: the call keeps the deposit");
    }
}

/// `issue_tx` for a cell created by `prev` (output 0) whose redeem script is `g.redeems[0]`, continuing at `succ_value`.
fn issue_tx_from(
    g: &Genesis,
    prev: &Transaction,
    recipient: &TokenState,
    next_issuer: &[u8],
    minter_call: bool,
    succ_value: u64,
) -> (Transaction, Vec<UtxoEntry>) {
    let (p, s) = kcc20_sil_cut();
    let (contract, entry) = if minter_call { ("PublicMint", "mint") } else { ("TokenSeed", "create") };
    let mut ss =
        encode_contract_entry_sig_script(&abi(F27f8403), contract, entry, &[kcc20_state_value(recipient), p.into(), s.into()])
            .unwrap();
    ss.extend(push_data(&g.redeems[0]));
    let mut inputs =
        vec![TransactionInput::new_with_compute_budget(TransactionOutpoint { transaction_id: prev.id(), index: 0 }, ss, 0, 0)];
    let mut entries = vec![UtxoEntry::new(prev.outputs[0].value, prev.outputs[0].script_public_key.clone(), 0, false, Some(g.cov))];
    fee_input(&mut inputs, &mut entries);
    let outputs = vec![
        TransactionOutput { value: succ_value, script_public_key: p2sh_spk(next_issuer), covenant: g.bind() },
        TransactionOutput { value: KAS, script_public_key: p2sh_spk(&holder_redeem(recipient)), covenant: g.bind() },
    ];
    (Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]), entries)
}

#[test]
fn the_kcc20_context_of_both_builds_is_the_one_pinned_handle() {
    // the issuers of both builds name the same holder program, whose handle is TemplateId::Kcc20PublicMint
    assert_eq!(hex(&kob_protocol::public_mint::kcc20_sil_hash()), KCC20_SIL_HASH);
    for v in IssuerVersion::ALL {
        let r = recommended(v).check().unwrap();
        assert_eq!(r.issuer.as_deref(), Some(v.label()));
        let st = kob_protocol::public_mint::minter_state_bytes_of(v, &minter(1, 1)).unwrap();
        assert_eq!(&st[1..33], &kcc20_hash());
    }
}

fn kcc20_hash() -> [u8; 32] {
    (0..32).map(|i| u8::from_str_radix(&KCC20_SIL_HASH[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>().try_into().unwrap()
}
