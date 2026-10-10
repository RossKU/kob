//! Genesis check of tokens issued the way upstream recommends for its published `KCC20PublicMint` app (argent-lang/
//! kcc20-reference `c8a0871`, README "Deployment and genesis"): a genesis group of one `PublicMint` (the advertised supply as
//! its allowance) and `TokenSeed` seeds, with or without pre-minted `KCC20` holders.
//!
//! Real fixtures, executed in rusty-kaspa v2.1.0's TxScriptEngine with KIP-20 covenant context: the actors are the programs
//! of upstream's published artifact (`contracts/third-party/kcc20-reference/public-mint.artifact.json`,
//! `kob_protocol::public_mint`), the holders KOB's pinned handle of the `KCC20` actor (`TemplateId::Kcc20PublicMint`).
//!
//!   - the genesis transaction passes the covenant context (one genesis group), and `verify_genesis` accepts it: the minters'
//!     allowance and the pre-minted amount are reported (max supply = supply + mint_allowance);
//!   - the genesis minter mints in the engine: the minted output is a holder of KOB's pinned template, the minter continues
//!     with the allowance less the amount, and the minted-so-far figure follows from the live minter's `remaining`;
//!     a seed creates a zero-amount holder; the two holders then transfer as `KCC20` holders of the token;
//!   - why the template fields are checked: a genesis minter whose `gen__kcc20_template` names another program mints that
//!     program (here an anyone-can-spend script carrying the token's covenant id) in the engine, and `verify_genesis`
//!     refuses it; so are minters without allowance semantics, a second extension commitment, foreign scripts, and the app's
//!     actors in a genesis of another program;
//!   - the registry record (`kob registry verify-genesis` over chain evidence): `mint_allowance`, and C2 (live mint
//!     authority) is undetermined while a minter lives, `[]` for a genesis of holders and seeds only; `official` needs it.
//!
//! Run: cargo test --release -p kob-tests --test kcc20_public_mint_genesis_tests -- --nocapture

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kaspa_consensus_core::tx::{
    CovenantBinding, ScriptPublicKey, Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::opcodes::codes::{OpDrop, OpTrue};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::genesis_evidence::{verify_token, TokenEvidence};
use kob_protocol::issue::TokenState;
use kob_protocol::public_mint::{
    decode_companion, minter_redeem, minter_state_bytes, public_mint, seed_redeem, token_seed, Companion, GenesisSupply, MinterState,
    SeedState, KCC20_SIL_HASH,
};
use kob_protocol::registry::{verify_genesis, verify_genesis_of, GenesisError, GenesisOutput, GenesisReport, Registry};
use kob_protocol::script::{p2sh_spk, push_data};
use serde_json::{json, Value};
use silverscript_abi::{encode_contract_entry_sig_script, ArtifactValue, SilAbiArtifact};

const PM: TemplateId = TemplateId::Kcc20PublicMint;
const KAS: u64 = 100_000_000;
const EXT: [u8; 32] = [0x5e; 32];
/// The covenant id that owns the holders here (owner scheme 0x04); an OP_TRUE input carrying it authorises them.
const OWNER_COV: [u8; 32] = [0xc0; 32];
const FUNDING: ([u8; 32], u32) = ([0x99; 32], 1);
const MINTER_KEY: [u8; 32] = [0x77; 32];

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn app() -> SilAbiArtifact {
    let doc: Value =
        serde_json::from_str(include_str!("../../../contracts/third-party/kcc20-reference/public-mint.artifact.json")).unwrap();
    serde_json::from_value(doc["sil_abi"].clone()).unwrap()
}

fn holder(amount: u64, ext: [u8; 32]) -> TokenState {
    TokenState { amount, owner: OWNER_COV, owner_scheme: 4, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment: ext }
}
fn holder_redeem(s: &TokenState) -> Vec<u8> {
    token_template(PM).redeem(&s.encode())
}
fn minter(remaining: i64, mint_amount: i64) -> MinterState {
    MinterState { remaining, mint_amount, owner: MINTER_KEY, extension_commitment: EXT }
}
fn seed() -> SeedState {
    SeedState { owner: MINTER_KEY, extension_commitment: EXT }
}
/// The `KCC20` program's Sil cut (what the app's actors pass as `gen__kcc20_prefix` / `gen__kcc20_suffix`): KOB's handle
/// without the context push.
fn kcc20_sil_cut() -> (Vec<u8>, Vec<u8>) {
    let h = token_template(PM);
    (h.prefix[..h.prefix.len() - 33].to_vec(), h.suffix.clone())
}

/// A genesis transaction: a plain OP_TRUE input authorises a group of P2SH outputs of the given redeem scripts.
struct Genesis {
    tx: Transaction,
    redeems: Vec<Vec<u8>>,
    cov: Hash,
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
    // consensus: one genesis group, authorised by input 0, and the input itself executes
    let entries = vec![UtxoEntry::new(100 * KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None)];
    let res = kob_protocol::verify::execute(&tx, &entries, false).expect("genesis covenant context");
    assert!(res.iter().all(|r| r.is_ok()), "{res:?}");
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
}

fn kcc20_state_value(s: &TokenState) -> ArtifactValue {
    s.to_abi()
}

/// Executes every input; `Ok` per input or the engine error.
fn exec(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    kob_protocol::verify::execute(tx, entries, false).expect("tx-level")
}

/// A fee input (OP_TRUE, no covenant) appended to a transaction.
fn fee_input(tx_inputs: &mut Vec<TransactionInput>, entries: &mut Vec<UtxoEntry>) {
    tx_inputs.push(TransactionInput::new_with_compute_budget(
        TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xfe; 32]), index: 0 },
        vec![],
        0,
        0,
    ));
    entries.push(UtxoEntry::new(50 * KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, None));
}

/// `PublicMint.mint` of the genesis minter at output `at`: the minter continues with `next_redeem`, the recipient output is
/// `recipient_redeem`; `cut` is the program the minter is told to validate the recipient against.
fn mint_tx(
    g: &Genesis,
    at: usize,
    recipient: &TokenState,
    next_redeem: &[u8],
    recipient_redeem: &[u8],
    cut: (Vec<u8>, Vec<u8>),
) -> (Transaction, Vec<UtxoEntry>) {
    let mut ss =
        encode_contract_entry_sig_script(&app(), "PublicMint", "mint", &[kcc20_state_value(recipient), cut.0.into(), cut.1.into()])
            .unwrap();
    ss.extend(push_data(&g.redeems[at]));
    let mut inputs = vec![TransactionInput::new_with_compute_budget(g.outpoint(at as u32), ss, 0, 0)];
    let mut entries = vec![g.entry(at)];
    fee_input(&mut inputs, &mut entries);
    let bind = Some(CovenantBinding { authorizing_input: 0, covenant_id: g.cov });
    let outputs = vec![
        TransactionOutput { value: g.tx.outputs[at].value, script_public_key: p2sh_spk(next_redeem), covenant: bind },
        TransactionOutput { value: KAS, script_public_key: p2sh_spk(recipient_redeem), covenant: bind },
    ];
    (Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]), entries)
}

/// `TokenSeed.create` of the genesis seed at output `at`: the seed continues unchanged, a zero-amount holder is created.
fn create_tx(g: &Genesis, at: usize, recipient: &TokenState) -> (Transaction, Vec<UtxoEntry>) {
    let (p, s) = kcc20_sil_cut();
    let mut ss =
        encode_contract_entry_sig_script(&app(), "TokenSeed", "create", &[kcc20_state_value(recipient), p.into(), s.into()]).unwrap();
    ss.extend(push_data(&g.redeems[at]));
    let mut inputs = vec![TransactionInput::new_with_compute_budget(g.outpoint(at as u32), ss, 0, 0)];
    let mut entries = vec![g.entry(at)];
    fee_input(&mut inputs, &mut entries);
    let bind = Some(CovenantBinding { authorizing_input: 0, covenant_id: g.cov });
    let outputs = vec![
        TransactionOutput {
            value: g.tx.outputs[at].value,
            script_public_key: g.tx.outputs[at].script_public_key.clone(),
            covenant: bind,
        },
        TransactionOutput { value: KAS, script_public_key: p2sh_spk(&holder_redeem(recipient)), covenant: bind },
    ];
    (Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]), entries)
}

/// Upstream's recommended genesis: one minter of the advertised supply, one seed, no pre-minted balance.
fn recommended() -> Genesis {
    genesis(vec![minter_redeem(&minter(21_000_000, 1_000)).unwrap(), seed_redeem(&seed())])
}

#[test]
fn the_actors_are_the_programs_of_the_published_artifact() {
    let a = app();
    for (name, p) in [("PublicMint", public_mint()), ("TokenSeed", token_seed())] {
        let c = &a.contracts[name];
        let bc = &c.compiled.bytecode;
        let (o, l) = (c.compiled.state_span.offset, c.compiled.state_span.len);
        assert_eq!((&bc[..o], l, &bc[o + l..]), (p.prefix.as_slice(), p.state_len, p.suffix.as_slice()), "{name}");
        assert_eq!(c.compiled.template_hash.as_slice(), p.sil_hash, "{name}");
    }
    // the KCC20 the actors validate against is the program KOB pins (its Sil hash is the context of every holder)
    assert_eq!(hex(&kob_protocol::public_mint::kcc20_sil_hash()), KCC20_SIL_HASH);
    assert_eq!(hex(&a.contracts["KCC20"].compiled.template_hash), KCC20_SIL_HASH);
    let (p, s) = kcc20_sil_cut();
    assert_eq!(hex(&silverscript_abi::template_hash(&p, &s)), KCC20_SIL_HASH);
}

#[test]
fn upstreams_recommended_genesis_verifies_and_reports_its_allowance() {
    let g = recommended();
    let r = g.check().expect("minter and seed of the published app");
    assert_eq!((r.supply, r.mint_allowance, r.minter_outputs.clone(), r.seed_outputs.clone()), (0, 21_000_000, vec![0], vec![1]));
    assert_eq!(r.genesis_supply().max_supply(), Some(21_000_000));

    // pre-minted holders, two minters (the allowance is their sum) and two seeds
    let g = genesis(vec![
        holder_redeem(&holder(500, EXT)),
        minter_redeem(&minter(1_000, 10)).unwrap(),
        seed_redeem(&seed()),
        minter_redeem(&minter(0, 10)).unwrap(),
        seed_redeem(&SeedState { owner: [0x01; 32], extension_commitment: EXT }),
        holder_redeem(&holder(250, EXT)),
    ]);
    let r = g.check().unwrap();
    assert_eq!((r.supply, r.mint_allowance), (750, 1_000));
    assert_eq!((r.minter_outputs.clone(), r.seed_outputs.clone()), (vec![1, 3], vec![2, 4]));
    assert_eq!(r.genesis_supply().max_supply(), Some(1_750));

    // the fixed-supply genesis `kob token issue --program public-mint` writes: holders only
    let g = genesis(vec![holder_redeem(&holder(1_000, EXT))]);
    let r = g.check().unwrap();
    assert_eq!((r.supply, r.mint_allowance, r.minter_outputs.len(), r.seed_outputs.len()), (1_000, 0, 0, 0));
}

#[test]
fn the_genesis_minter_mints_holders_of_the_pinned_template_and_the_seed_creates_receivers() {
    let g = recommended();
    let report = g.check().unwrap();
    let cut = kcc20_sil_cut();

    // mint 1,000 (the per-mint limit) into a holder owned by OWNER_COV
    let minted = holder(1_000, EXT);
    let next = minter_redeem(&minter(21_000_000 - 1_000, 1_000)).unwrap();
    let (tx, entries) = mint_tx(&g, 0, &minted, &next, &holder_redeem(&minted), cut.clone());
    let res = exec(&tx, &entries);
    assert!(res.iter().all(|r| r.is_ok()), "mint: {res:?}");
    // the minted output is an instance of KOB's pinned handle (orders custody and fill it like any KCC20PublicMint holder)
    assert_eq!(token_template(PM).state_of(&holder_redeem(&minted)), Some(minted.encode().as_slice()));
    // minted so far: the max supply less what the live minter has left
    assert_eq!(report.genesis_supply().minted(&[21_000_000 - 1_000]), Some(1_000));

    // the minter enforces its own rules: more than the per-mint limit, a wrong continuation, another extension
    let big = holder(1_001, EXT);
    let (tx, e) = mint_tx(&g, 0, &big, &minter_redeem(&minter(21_000_000 - 1_001, 1_000)).unwrap(), &holder_redeem(&big), cut.clone());
    assert!(exec(&tx, &e)[0].is_err(), "above mint_amount");
    let (tx, e) = mint_tx(&g, 0, &minted, &minter_redeem(&minter(21_000_000, 1_000)).unwrap(), &holder_redeem(&minted), cut.clone());
    assert!(exec(&tx, &e)[0].is_err(), "allowance not reduced");
    let other = holder(1_000, [0x01; 32]);
    let (tx, e) = mint_tx(&g, 0, &other, &next, &holder_redeem(&other), cut.clone());
    assert!(exec(&tx, &e)[0].is_err(), "another extension commitment");

    // the seed creates a zero-amount holder of the token
    let zero = holder(0, EXT);
    let (tx2, e2) = create_tx(&g, 1, &zero);
    let res = exec(&tx2, &e2);
    assert!(res.iter().all(|r| r.is_ok()), "create: {res:?}");
    let (tx3, e3) = create_tx(&g, 1, &holder(1, EXT));
    assert!(exec(&tx3, &e3)[0].is_err(), "a seed creates no amount");

    // both new holders are KCC20 holders of the token: the minted one leads a transfer with the seeded one as delegator
    let a = app();
    let merged = holder(1_000, EXT);
    let leader_ss = {
        let mut s = encode_contract_entry_sig_script(
            &a,
            "KCC20",
            "transfer",
            &[ArtifactValue::Array(vec![merged.to_abi()]), vec![0x00u8].into()],
        )
        .unwrap();
        s.extend(push_data(&holder_redeem(&minted)));
        s
    };
    let delegate_ss = {
        let mut s = encode_contract_entry_sig_script(&a, "KCC20", "transfer_delegator", &[Vec::<u8>::new().into()]).unwrap();
        s.extend(push_data(&holder_redeem(&zero)));
        s
    };
    let inputs = vec![
        TransactionInput::new_with_compute_budget(TransactionOutpoint { transaction_id: tx.id(), index: 1 }, leader_ss, 0, 0),
        TransactionInput::new_with_compute_budget(TransactionOutpoint { transaction_id: tx2.id(), index: 1 }, delegate_ss, 0, 0),
        TransactionInput::new_with_compute_budget(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xc1; 32]), index: 0 },
            vec![],
            0,
            0,
        ),
    ];
    let entries = vec![
        UtxoEntry::new(KAS, p2sh_spk(&holder_redeem(&minted)), 0, false, Some(g.cov)),
        UtxoEntry::new(KAS, p2sh_spk(&holder_redeem(&zero)), 0, false, Some(g.cov)),
        UtxoEntry::new(KAS, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(Hash::from_bytes(OWNER_COV))),
    ];
    let outputs = vec![TransactionOutput {
        value: KAS,
        script_public_key: p2sh_spk(&holder_redeem(&merged)),
        covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: g.cov }),
    }];
    let t = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let res = exec(&t, &entries);
    assert!(res.iter().all(|r| r.is_ok()), "transfer of the minted and seeded holders: {res:?}");
}

/// A minter's `gen__kcc20_template` decides what it mints. One that names another program mints that program under the
/// token's covenant id; here an anyone-can-spend script, accepted by the minter in the engine. The genesis check refuses
/// such a minter, so a token whose genesis holds it is never `genesis_verified`.
#[test]
fn a_minter_naming_another_program_mints_it_and_the_genesis_check_refuses_it() {
    // an anyone-can-spend "program": the state pushes, then 7 x OP_DROP, OP_TRUE
    let (bad_prefix, bad_suffix) = (vec![OpTrue, OpDrop], [vec![OpDrop; 7], vec![OpTrue]].concat());
    let bad_tpl = silverscript_abi::template_hash(&bad_prefix, &bad_suffix);
    let with_kcc20_tpl = |m: &MinterState| {
        let mut st = minter_state_bytes(m).unwrap();
        st[1..33].copy_from_slice(&bad_tpl);
        public_mint().redeem(&st)
    };
    let m = minter(1_000_000, 1_000_000);
    let g = genesis(vec![with_kcc20_tpl(&m), seed_redeem(&seed())]);
    assert!(matches!(g.check(), Err(GenesisError::BadState(0, ref e)) if e.contains("gen__kcc20_template")), "{:?}", g.check());

    // in the engine it mints the backdoor: the recipient is `bad_prefix ‖ 0x20 bad_tpl ‖ KCC20State ‖ bad_suffix`
    let loot = holder(1_000_000, EXT);
    let backdoor = [bad_prefix.clone(), vec![0x20], bad_tpl.to_vec(), loot.encode(), bad_suffix.clone()].concat();
    let next = with_kcc20_tpl(&minter(0, 1_000_000));
    let (tx, e) = mint_tx(&g, 0, &loot, &next, &backdoor, (bad_prefix, bad_suffix));
    let res = exec(&tx, &e);
    assert!(res.iter().all(|r| r.is_ok()), "the foreign minter mints its own program: {res:?}");
    // and that output, carrying the token's covenant id, is spendable by anyone
    let spend = Transaction::new(
        1,
        vec![TransactionInput::new_with_compute_budget(
            TransactionOutpoint { transaction_id: tx.id(), index: 1 },
            push_data(&backdoor),
            0,
            0,
        )],
        vec![TransactionOutput { value: KAS / 2, script_public_key: ScriptPublicKey::new(0, vec![OpTrue].into()), covenant: None }],
        0,
        Default::default(),
        0,
        vec![],
    );
    let res = exec(&spend, &[UtxoEntry::new(KAS, p2sh_spk(&backdoor), 0, false, Some(g.cov))]);
    assert!(res[0].is_ok(), "{res:?}");
    assert_eq!(decode_companion(&backdoor).unwrap(), None, "not an actor of the app");

    // the minter's own template field, too
    let mut st = minter_state_bytes(&m).unwrap();
    st[34..66].copy_from_slice(&[0x42; 32]);
    let g = genesis(vec![public_mint().redeem(&st)]);
    assert!(matches!(g.check(), Err(GenesisError::BadState(0, _))));
}

#[test]
fn the_genesis_check_refuses_what_is_not_a_clean_issuance_of_the_app() {
    let m = || minter_redeem(&minter(1_000, 10)).unwrap();
    // a minter that can never mint, or a negative allowance
    for (remaining, mint_amount) in [(1_000, 0), (1_000, -1), (-1, 10)] {
        let g = genesis(vec![minter_redeem(&minter(remaining, mint_amount)).unwrap(), seed_redeem(&seed())]);
        assert!(matches!(g.check(), Err(GenesisError::BadState(0, ref e)) if e.contains("not a minter")), "{remaining}/{mint_amount}");
    }
    // a second extension commitment (a seed or minter of another class)
    let g = genesis(vec![m(), seed_redeem(&SeedState { owner: MINTER_KEY, extension_commitment: [0x01; 32] })]);
    assert!(matches!(g.check(), Err(GenesisError::ExtensionCommitment(1, ..))));
    let g = genesis(vec![
        holder_redeem(&holder(5, EXT)),
        minter_redeem(&MinterState { extension_commitment: [0; 32], ..minter(9, 1) }).unwrap(),
    ]);
    assert!(matches!(g.check(), Err(GenesisError::ExtensionCommitment(1, ..))));
    // a hidden script beside the actors
    let g = genesis(vec![m(), seed_redeem(&seed()), vec![OpTrue]]);
    assert_eq!(g.check(), Err(GenesisError::NotTemplate(2, "kcc20-ref-public-mint".into())));
    // the app's actors in the genesis of another program are foreign outputs there
    let g = genesis(vec![token_template(TemplateId::Kcc20Ref).redeem(&holder(5, EXT).encode()), m()]);
    assert_eq!(g.check_as("kcc20-ref-3x3"), Err(GenesisError::NotTemplate(1, "kcc20-ref-3x3".into())));
    // an allowance that does not fit 64 bits beside the holders
    let g = genesis(vec![holder_redeem(&holder(i64::MAX as u64, EXT)), m()]);
    assert!(matches!(g.check(), Err(GenesisError::BadState(..))));
    // verify_genesis without a named commitment takes the first output's
    let g = recommended();
    let r = Registry::default_registry();
    let rep = verify_genesis(
        r.template("kcc20-ref-public-mint").unwrap(),
        &hex(&g.cov.as_bytes()),
        &hex(&g.tx.id().as_bytes()),
        FUNDING,
        &g.outputs(),
    )
    .unwrap();
    assert_eq!(rep.mint_allowance, 21_000_000);
    assert!(matches!(decode_companion(&g.redeems[0]).unwrap(), Some(Companion::Minter(_))));
}

// ---------------------------------------------------------------- the registry record

fn evidence_of(g: &Genesis, live_minters: Option<Vec<(String, u32)>>) -> TokenEvidence {
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
        "live_minters": live_minters.map(|l| l.into_iter().map(|(t, i)| json!({"txid": t, "index": i})).collect::<Vec<_>>()),
    }))
    .unwrap()
}

fn registry_with(g: &Genesis) -> Registry {
    let mut r = Registry::default_registry();
    r.tokens.push(
        serde_json::from_value(json!({
            "ticker": "PMT", "name": "Public mint test", "family": "kcc20", "covenant_id": hex(&g.cov.as_bytes()),
            "template_id": "kcc20-ref-public-mint", "extension_commitment": hex(&EXT), "extension_class": "other",
            "decimals": 0, "status": "pending-review", "verified": false,
        }))
        .unwrap(),
    );
    r
}

/// What `kob registry verify-genesis` records for such a token, and when it can be official: C2 (no live mint authority) is
/// undetermined while a genesis minter's lineage is not traced, and holds when the trace finds no minter with allowance
/// left (minting closed: the supply is final at `supply + mint_allowance`); a genesis without a minter proves it.
#[test]
fn the_registry_record_carries_the_allowance_and_official_waits_for_the_end_of_minting() {
    let g = recommended();
    let reg = registry_with(&g);
    let rec = verify_token(&reg, &evidence_of(&g, None), 2_000).result.expect("verified");
    assert_eq!((rec.supply, rec.mint_allowance, rec.minter_outputs.clone()), (0, Some(21_000_000), vec![0]));
    assert_eq!(rec.live_minters, None, "a live minter may mint: C2 undetermined until traced");

    // the record in the registry: official is refused while C2 is undetermined. (Listing needs a reviewed template: the
    // shipped kcc20-ref-public-mint stays pending review, so this registry marks it reviewed for the rule under test.)
    let mut reg2 = reg.clone();
    for t in reg2.templates.iter_mut().filter(|t| t.id == "kcc20-ref-public-mint") {
        t.review_status = kob_protocol::registry::ReviewStatus::Reviewed;
    }
    let t = reg2.tokens.last_mut().unwrap();
    t.verified = true;
    t.genesis_verified = Some(true);
    t.status = kob_protocol::registry::Status::Listed;
    t.genesis = Some(rec.clone());
    reg2.validate().expect("listed with an undetermined C2");
    reg2.tokens.last_mut().unwrap().official = true;
    assert!(reg2.validate().is_err(), "official needs live_minters: []");

    // the trace found the minters exhausted (no live minter with allowance): C2 holds and the token can be official
    let traced = verify_token(&reg, &evidence_of(&g, Some(vec![])), 2_000).result.unwrap();
    assert_eq!(traced.live_minters, Some(vec![]));
    reg2.tokens.last_mut().unwrap().genesis = Some(traced);
    reg2.validate().expect("official once minting is closed");
    // a negative allowance or one without minters is refused by the validator
    let mut bad = reg2.clone();
    bad.tokens.last_mut().unwrap().genesis.as_mut().unwrap().mint_allowance = Some(-1);
    assert!(bad.validate().is_err());
    let mut bad = reg2.clone();
    bad.tokens.last_mut().unwrap().genesis.as_mut().unwrap().minter_outputs.clear();
    assert!(bad.validate().is_err());

    // holders (and seeds) only: a fixed supply, C2 holds from the genesis alone
    let fixed = genesis(vec![holder_redeem(&holder(1_000, EXT)), seed_redeem(&seed())]);
    let rec = verify_token(&registry_with(&fixed), &evidence_of(&fixed, None), 2_000).result.unwrap();
    assert_eq!((rec.supply, rec.mint_allowance, rec.live_minters), (1_000, None, Some(vec![])));

    // a contaminated genesis does not verify
    let dirty = genesis(vec![minter_redeem(&minter(10, 1)).unwrap(), vec![OpTrue]]);
    let e = verify_token(&registry_with(&dirty), &evidence_of(&dirty, None), 2_000).result.unwrap_err();
    assert!(e.contains("not an instance"), "{e}");

    // the supply arithmetic of such a token over its life
    let s = GenesisSupply { holders: 0, mint_allowance: 21_000_000 };
    assert_eq!(
        (s.minted(&[21_000_000]), s.minted(&[20_000_000, 500_000]), s.minted(&[0])),
        (Some(0), Some(500_000), Some(21_000_000))
    );
}
