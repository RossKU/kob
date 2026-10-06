//! Fixed-supply KCC-20 issuance (`kob token issue` / `kob_protocol::issue`) against rusty-kaspa
//! v2.1.0's script engine: the genesis transaction, then first transfers of the issued token UTXOs
//! (owner scheme 0x04 custody, up to 8 token inputs and 8 token outputs) and the limits.
//!
//! Run: cargo test --release -p kob-tests --test kcc20_issue_tests -- --nocapture --test-threads=1

mod common;

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::mass::transaction_estimated_serialized_size;
use kaspa_consensus_core::tx::{
    CovenantBinding, MutableTransaction, PopulatedTransaction, ScriptPublicKey, Transaction, TransactionId, TransactionInput,
    TransactionOutpoint, TransactionOutput, UtxoEntry,
};
use kaspa_consensus_core::Hash;
use kaspa_txscript::caches::Cache;
use kaspa_txscript::covenants::CovenantsContext;
use kaspa_txscript::opcodes::codes::OpTrue;
use kaspa_txscript::{pay_to_script_hash_script, EngineCtx, EngineFlags, TxScriptEngine};
use kaspa_txscript_errors::TxScriptError;
use kob_protocol::issue::{
    build_genesis, verify_scheme4_enabled, FundingUtxo, GenesisPlan, Holder, IssueError, IssueSpec, Program, TokenState,
    EXTENSION_FIXED_SUPPLY, SCHEME_COVENANT_ID, SCHEME_P2PK_SCHNORR,
};
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::ArtifactValue;
use silverscript_lang::compiler::CompileOptions;

use common::{bytecode, compile_contract, compiled_template_parts_and_hash};

const KAS: u64 = 100_000_000;
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const FREE_UNITS: u64 = 9_999;
const UNITS_PER_BUDGET: u64 = 10_000;
const NET_FEE: u64 = KAS / 10;

fn secret() -> SecretKey {
    let mut rng = thread_rng();
    let mut sk = [0u8; 32];
    loop {
        rng.fill_bytes(&mut sk);
        if let Ok(s) = SecretKey::from_slice(&sk) {
            return s;
        }
    }
}
fn keypair(sk: &SecretKey) -> Keypair {
    Keypair::from_secret_key(&Secp256k1::new(), sk)
}
fn pk(kp: &Keypair) -> [u8; 32] {
    kp.x_only_public_key().0.serialize()
}

/// Issue `holders` (one output each) from a fresh funding UTXO and return the signed, verified plan.
fn issue(holders: Vec<Holder>, funder: &SecretKey) -> GenesisPlan {
    let supply: u64 = holders.iter().map(|h| h.amount).sum();
    let spec = IssueSpec::new(
        "Engine Test Token",
        "ENGT",
        8,
        supply,
        holders,
        vec![FundingUtxo {
            outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([0x5a; 32]), index: 3 },
            amount: 1_000 * KAS,
            owner_pubkey: pk(&keypair(funder)),
        }],
    );
    let mut plan = build_genesis(&spec).expect("plan");
    plan.sign(funder).expect("sign");
    let report = plan.verify().expect("genesis verifies");
    assert!(report.scripts_executed);
    plan
}

// ---------------------------------------------------------------- transfers

#[derive(Clone)]
struct TokIn {
    outpoint: TransactionOutpoint,
    value: u64,
    state: TokenState,
    owner: Option<Keypair>, // None = covenant-owned (scheme 4)
}

/// The genesis outputs as spendable token UTXOs. `owners[i]` is the key of output i (None for scheme 4).
fn utxos_of(plan: &GenesisPlan, owners: &[Option<Keypair>]) -> Vec<TokIn> {
    plan.token_outpoints()
        .into_iter()
        .enumerate()
        .map(|(i, outpoint)| TokIn { outpoint, value: plan.tx.outputs[i].value, state: plan.states[i].clone(), owner: owners[i] })
        .collect()
}

struct Transfer {
    tx: Transaction,
    entries: Vec<UtxoEntry>,
}

/// Transfer `ins` (input 0 is the leader) into `outs`. `extra` = additional plain covenant inputs
/// (`OpTrue` UTXOs carrying the given covenant id, e.g. the owner covenant of a scheme-4 state).
fn transfer(program: &Program, cov: Hash, ins: &[TokIn], outs: &[TokenState], extra: &[Hash], budgets: &[u16]) -> Transfer {
    let total: u64 = ins.iter().map(|i| i.value).sum();
    let n = outs.len() as u64;
    let per = (total - NET_FEE) / n;
    let outputs: Vec<TransactionOutput> = outs
        .iter()
        .enumerate()
        .map(|(k, s)| TransactionOutput {
            value: if k == 0 { total - NET_FEE - per * (n - 1) } else { per },
            script_public_key: program.spk(s),
            covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: cov }),
        })
        .collect();
    let mut entries: Vec<UtxoEntry> = ins
        .iter()
        .map(|i| UtxoEntry::new(i.value, pay_to_script_hash_script(&program.redeem(&i.state)), 0, false, Some(cov)))
        .collect();
    let mut inputs: Vec<TransactionInput> = ins
        .iter()
        .enumerate()
        .map(|(k, i)| TransactionInput::new_with_compute_budget(i.outpoint, vec![], 0, budgets.get(k).copied().unwrap_or(0)))
        .collect();
    for (j, c) in extra.iter().enumerate() {
        entries.push(UtxoEntry::new(1_000, ScriptPublicKey::new(0, vec![OpTrue].into()), 0, false, Some(*c)));
        inputs.push(TransactionInput::new_with_compute_budget(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes([0x90 + j as u8; 32]), index: 0 },
            vec![],
            0,
            0,
        ));
    }
    let mut tx = Transaction::new(1, inputs, outputs, 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (k, i) in ins.iter().enumerate() {
        let w = match &i.owner {
            Some(kp) => sign(&unsigned, &entries, k, kp),
            None => vec![],
        };
        let redeem = program.redeem(&i.state);
        tx.inputs[k].signature_script = if k == 0 {
            program.leader_sig_script(&redeem, outs, [vec![0x00], w].concat()).expect("leader sigscript")
        } else {
            program.delegator_sig_script(&redeem, w).expect("delegator sigscript")
        };
    }
    Transfer { tx, entries }
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(0x01).unwrap(), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).unwrap();
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(0x01);
    s
}

fn execute(t: &Transfer) -> Result<Vec<Result<u64, TxScriptError>>, String> {
    let populated = PopulatedTransaction::new(&t.tx, t.entries.clone());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..t.tx.inputs.len())
        .map(|i| {
            let input = t.tx.inputs[i].clone();
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &input,
                i,
                &t.entries[i],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
            );
            vm.execute().map(|_| vm.used_script_units().0)
        })
        .collect())
}

/// Two-pass positive run: every input must pass, budgets are derived from the measured units.
fn run_ok(name: &str, program: &Program, cov: Hash, ins: &[TokIn], outs: &[TokenState], extra: &[Hash]) {
    let first = transfer(program, cov, ins, outs, extra, &[]);
    let res = execute(&first).unwrap_or_else(|e| panic!("{name}: {e}"));
    let mut budgets = vec![];
    for (i, r) in res.iter().enumerate() {
        match r {
            Ok(u) => budgets.push(u.saturating_sub(FREE_UNITS).div_ceil(UNITS_PER_BUDGET) as u16),
            Err(e) => panic!("{name}: input {i} failed: {e:?}"),
        }
    }
    let second = transfer(program, cov, ins, outs, extra, &budgets);
    let res = execute(&second).expect("ctx");
    assert!(res.iter().all(Result::is_ok), "{name}: second pass failed");
    println!(
        "TRANSFER {name} [PASS] inputs={} outputs={} tx_size={} B, script_units per input: {:?}",
        second.tx.inputs.len(),
        second.tx.outputs.len(),
        transaction_estimated_serialized_size(&second.tx),
        res.iter().map(|r| *r.as_ref().unwrap()).collect::<Vec<_>>()
    );
}

/// Negative run: some input must be rejected (script or covenant context).
fn run_bad(name: &str, program: &Program, cov: Hash, ins: &[TokIn], outs: &[TokenState], extra: &[Hash]) {
    let t = transfer(program, cov, ins, outs, extra, &[]);
    match execute(&t) {
        Err(e) => println!("NEGATIVE {name} [REJECTED by covenant context: {e}]"),
        Ok(res) => {
            let bad: Vec<usize> = res.iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect();
            assert!(!bad.is_empty(), "{name}: expected a rejection but every input passed");
            println!("NEGATIVE {name} [REJECTED at inputs {bad:?}]");
        }
    }
}

fn held_by(kp: &Keypair, amount: u64) -> Holder {
    Holder::new(pk(kp), SCHEME_P2PK_SCHNORR, amount)
}
fn next_state(from: &TokenState, amount: u64, owner: [u8; 32], scheme: u8) -> TokenState {
    TokenState { amount, owner, owner_scheme: scheme, ..from.clone() }
}

// ---------------------------------------------------------------- tests

#[test]
fn issue_splice_codec_matches_the_compiler() {
    let p = Program::kcc20_8x8().unwrap();
    let st = TokenState {
        amount: 123_456_789,
        owner: [0xab; 32],
        owner_scheme: SCHEME_COVENANT_ID,
        borrow_scheme: 0,
        borrow_guard: [0u8; 32],
        extension_commitment: EXTENSION_FIXED_SUPPLY,
    };
    let art = compile_contract(
        &common::contract_source("KCC20Ref_8x8"),
        &[
            ArtifactValue::Int(st.amount as i64),
            st.owner.to_vec().into(),
            ArtifactValue::Byte(st.owner_scheme),
            ArtifactValue::Byte(st.borrow_scheme),
            st.borrow_guard.to_vec().into(),
            st.extension_commitment.to_vec().into(),
        ],
        CompileOptions::default(),
    )
    .expect("compile KCC20Ref_8x8");
    assert_eq!(p.redeem(&st), bytecode(&art), "issued redeem script = compiler output for the same state");
    let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
    assert_eq!((p.prefix.clone(), p.suffix.clone(), p.template_hash.to_vec()), (prefix, suffix, hash));
    verify_scheme4_enabled(&p).expect("owner scheme 0x04 is enabled");
    println!(
        "program {} template_hash={} prefix={} B suffix={} B",
        p.name,
        kob_protocol::issue::hex(&p.template_hash),
        p.prefix.len(),
        p.suffix.len()
    );
}

#[test]
fn issue_genesis_then_first_transfers() {
    let funder = secret();
    let (alice, bob) = (keypair(&secret()), keypair(&secret()));
    let owner_cov = Hash::from_bytes([0xc7; 32]);
    let plan =
        issue(vec![held_by(&alice, 600), held_by(&bob, 300), Holder::new(owner_cov.as_bytes(), SCHEME_COVENANT_ID, 100)], &funder);
    let p = &plan.program;
    let cov = plan.covenant_id;
    let ins = utxos_of(&plan, &[Some(alice), Some(bob), None]);
    println!("GENESIS covenant_id={cov} txid={} outputs={} fee={} sompi", plan.txid(), plan.states.len(), plan.fee);

    // alice sends 250 to bob and 350 back to herself
    run_ok(
        "alice splits 600 into 250 (bob) + 350 (alice)",
        p,
        cov,
        &ins[..1],
        &[next_state(&ins[0].state, 250, pk(&bob), 0), next_state(&ins[0].state, 350, pk(&alice), 0)],
        &[],
    );
    // alice deposits 100 into a covenant-owned state (scheme 0x04, borrow disabled): the custody a KOB ask uses
    let escrow = Hash::from_bytes([0xe5; 32]);
    run_ok(
        "alice deposits 100 into a scheme-0x04 owner covenant",
        p,
        cov,
        &ins[..1],
        &[next_state(&ins[0].state, 100, escrow.as_bytes(), SCHEME_COVENANT_ID), next_state(&ins[0].state, 500, pk(&alice), 0)],
        &[],
    );
    // merge alice + bob
    run_ok("alice and bob merge 900", p, cov, &ins[..2], &[next_state(&ins[0].state, 900, pk(&alice), 0)], &[]);
    // a scheme-0x04 holding is spent by a transaction that also spends an input of its owner covenant
    run_ok(
        "covenant-held 100 moves when the owner covenant is in the transaction",
        p,
        cov,
        &ins[2..3],
        &[next_state(&ins[2].state, 100, pk(&alice), 0)],
        &[owner_cov],
    );
    run_bad(
        "covenant-held 100 without the owner covenant in the transaction",
        p,
        cov,
        &ins[2..3],
        &[next_state(&ins[2].state, 100, pk(&alice), 0)],
        &[],
    );
}

#[test]
fn issue_eight_inputs_and_eight_outputs() {
    let funder = secret();
    let keys: Vec<Keypair> = (0..9).map(|_| keypair(&secret())).collect();
    let plan = issue(keys.iter().map(|k| held_by(k, 100)).collect(), &funder);
    assert!(plan.warnings.iter().any(|w| w.contains("9 genesis outputs exceed")), "{:?}", plan.warnings);
    let p = &plan.program;
    let cov = plan.covenant_id;
    let ins = utxos_of(&plan, &keys.iter().map(|k| Some(*k)).collect::<Vec<_>>());

    // 8 token inputs (leader + 7 delegators) -> 1 output
    run_ok("8 inputs -> 1 output", p, cov, &ins[..8], &[next_state(&ins[0].state, 800, pk(&keys[0]), 0)], &[]);
    // 8 inputs -> 8 outputs
    let outs8: Vec<TokenState> = (0..8).map(|i| next_state(&ins[0].state, 100, pk(&keys[i]), 0)).collect();
    run_ok("8 inputs -> 8 outputs", p, cov, &ins[..8], &outs8, &[]);
    // 1 input -> 8 outputs
    let outs8b: Vec<TokenState> = (0..8).map(|i| next_state(&ins[0].state, if i == 0 { 30 } else { 10 }, pk(&keys[i]), 0)).collect();
    run_ok("1 input -> 8 outputs", p, cov, &ins[..1], &outs8b, &[]);

    // limits: 9 inputs, 9 outputs, conservation
    run_bad("9 inputs -> 1 output (limit 8)", p, cov, &ins[..9], &[next_state(&ins[0].state, 900, pk(&keys[0]), 0)], &[]);
    let outs9: Vec<TokenState> = (0..9).map(|i| next_state(&ins[0].state, if i == 0 { 20 } else { 10 }, pk(&keys[i]), 0)).collect();
    run_bad("1 input -> 9 outputs (limit 8)", p, cov, &ins[..1], &outs9[..9], &[]);
    run_bad("mint +1 (conservation)", p, cov, &ins[..2], &[next_state(&ins[0].state, 201, pk(&keys[0]), 0)], &[]);
    run_bad("burn -1 (conservation)", p, cov, &ins[..2], &[next_state(&ins[0].state, 199, pk(&keys[0]), 0)], &[]);
    let mut bad_scheme = next_state(&ins[0].state, 200, pk(&keys[0]), 0);
    bad_scheme.owner_scheme = 0x7f;
    run_bad("unsupported owner scheme 0x7f", p, cov, &ins[..2], &[bad_scheme], &[]);
    let mut ext = next_state(&ins[0].state, 200, pk(&keys[0]), 0);
    ext.extension_commitment = [0xee; 32];
    run_bad("extension commitment not preserved", p, cov, &ins[..2], &[ext], &[]);
    // the wrong owner cannot spend
    let mut stolen = ins[0].clone();
    stolen.owner = Some(keys[1]);
    run_bad("spend without the owner's signature", p, cov, &[stolen], &[next_state(&ins[0].state, 100, pk(&keys[1]), 0)], &[]);
}

#[test]
fn issue_tool_rules() {
    let funder = secret();
    let f = FundingUtxo {
        outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([1; 32]), index: 0 },
        amount: 500 * KAS,
        owner_pubkey: pk(&keypair(&funder)),
    };
    let own = f.owner_pubkey;
    // borrow on a covenant-held state is rejected by the tool, with or without the allow flag
    for allow in [false, true] {
        let mut h = Holder::new([0xc4; 32], SCHEME_COVENANT_ID, 1000);
        h.borrow_scheme = 1;
        h.borrow_guard = [1; 32];
        let mut spec = IssueSpec::new("T", "T1", 0, 1000, vec![h], vec![f.clone()]);
        spec.allow_borrow = allow;
        assert!(matches!(build_genesis(&spec).unwrap_err(), IssueError::Borrow(_)), "allow={allow}");
    }
    // supply mismatch
    let spec = IssueSpec::new("T", "T1", 0, 1000, vec![Holder::new(own, 0, 999)], vec![f.clone()]);
    assert!(matches!(build_genesis(&spec).unwrap_err(), IssueError::SupplyMismatch { sum: 999, declared: 1000 }));
    // more than 8 outputs is flagged (warning), not silently accepted
    let spec = IssueSpec::new("T", "T1", 0, 90, (0..9).map(|i| Holder::new([i + 1; 32], 0, 10)).collect(), vec![f.clone()]);
    let plan = build_genesis(&spec).unwrap();
    assert!(plan.warnings.iter().any(|w| w.contains("exceed the 8-output")));
    // the program cannot mint: no entry other than transfer / transfer_delegator (supply = sum of genesis outputs)
    let p = Program::kcc20_8x8().unwrap();
    let redeem = p.redeem(&plan.states[0]);
    assert!(p.leader_sig_script(&redeem, &plan.states, vec![0]).is_ok());
    assert_eq!(plan.states.iter().map(|s| s.amount).sum::<u64>(), plan.spec.supply);
    // carrier so small that the storage mass explodes is refused by the mass check
    let mut spec = IssueSpec::new("T", "T1", 0, 90, (0..9).map(|i| Holder::new([i + 1; 32], 0, 10)).collect(), vec![f.clone()]);
    spec.carrier = KAS / 10;
    assert!(build_genesis(&spec).is_err(), "9 outputs of 0.1 KAS exceed the standard mass");
}
