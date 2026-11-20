//! Tests the automatic creator lock (`kob_protocol::lock_auto`, `contracts/locks/KobLockAuto.sil`)
//! in the rusty-kaspa v2.1.0 script engine.
//!
//! A genesis locks a share under the owner `Blake2b(KobLockAuto script)` and pays a plain KAS coin
//! to that script's address. After the lock time anyone may send the release, which spends the coin
//! and the locked UTXO and pays both to the creator. These tests show that the release needs no
//! signature, and that every other shape is refused by the lock script: an earlier lock time, the
//! tokens or the KAS sent elsewhere, a part of the tokens, a coin or a token UTXO of another
//! transaction, a covenant input on the side, more inputs, the coin at another index, a second
//! token output, the KAS of the token output taken, and more than `MAX_TAKE` kept from the coin.

use std::collections::BTreeMap;

use kaspa_consensus_core::tx::{Transaction, TransactionId, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::TokenRef;
use kob_protocol::creator_lock::{verify_genesis_lock, CreatorLock, LockKind};
use kob_protocol::issue::{build_creation, CreationPlan, FundingUtxo, Holder, IssueProgram, IssueSpec};
use kob_protocol::lock_auto::{auto_budget, build_release, AutoLock, ReleaseAutoLock, AUTO_COIN, MAX_TAKE, RELEASE_TIP};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{Kcc20State, TokenState};
use kob_protocol::tx::{finalize, pubkey_of, sign_locally, BuiltTx, FinalizeOptions, KeyUtxo, SigPlan, TokenUtxo, Utxo, Witness};
use secp256k1::SecretKey;

const KAS: u64 = 100_000_000;
const SECRET: [u8; 32] = [0x41; 32];
const KEEPER: [u8; 32] = [0x42; 32];
/// 2027-01-01T00:00:00Z
const UNTIL: u64 = 1_798_761_600_000;
const FREE: u64 = 800_000;
const LOCKED: u64 = 200_000;

struct Launch {
    plan: CreationPlan,
    lock: CreatorLock,
    auto: AutoLock,
    program: TemplateId,
}

fn key() -> [u8; 32] {
    pubkey_of(&SECRET).unwrap()
}

fn keeper() -> [u8; 32] {
    pubkey_of(&KEEPER).unwrap()
}

fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    [(key(), SECRET), (keeper(), KEEPER)].into_iter().collect()
}

/// A signed creation: output 0 free to the key, output 1 locked until [`UNTIL`], output 2 the coin.
fn launch(program: IssueProgram) -> Launch {
    let lock = CreatorLock::new(UNTIL, key()).unwrap();
    let auto = AutoLock::new(lock, program.template_id()).unwrap();
    let funding = FundingUtxo {
        outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([7; 32]), index: 0 },
        amount: 500 * KAS,
        owner_pubkey: key(),
    };
    let holders = vec![Holder::new(key(), 0, FREE), Holder::new(auto.authority(), 0x03, LOCKED)];
    let mut spec = IssueSpec::new("Locked", "LOCK", 2, FREE + LOCKED, holders, vec![funding]);
    spec.program = program;
    spec.lock = Some(lock);
    spec.lock_kind = LockKind::Auto;
    let mut plan = build_creation(&spec).unwrap();
    plan.sign(&SecretKey::from_slice(&SECRET).unwrap()).unwrap();
    plan.verify().unwrap();
    Launch { plan, lock, auto, program: program.template_id() }
}

fn locked_utxo(l: &Launch) -> TokenUtxo {
    let g = &l.plan.genesis;
    TokenUtxo {
        utxo: Utxo {
            transaction_id: g.txid().as_bytes(),
            index: 1,
            amount: g.tx.outputs[1].value,
            block_daa_score: 0,
            covenant_id: Some(g.covenant_id.as_bytes()),
        },
        state: TokenState::Kcc20(l.auto.locked_state(LOCKED as i64, [0; 32])),
    }
}

fn coin(l: &Launch) -> Utxo {
    let g = &l.plan.genesis;
    Utxo { transaction_id: g.txid().as_bytes(), index: 2, amount: g.tx.outputs[2].value, block_daa_score: 0, covenant_id: None }
}

fn request(l: &Launch) -> ReleaseAutoLock {
    ReleaseAutoLock {
        token: TokenRef { covenant_id: l.plan.genesis.covenant_id.as_bytes(), program: l.program },
        lock: l.lock,
        tokens: locked_utxo(l),
        coin: coin(l),
        keeper: None,
        tip: 0,
        fee: Default::default(),
    }
}

fn keeper_coin() -> KeyUtxo {
    KeyUtxo {
        utxo: Utxo { transaction_id: [3; 32], index: 1, amount: 5 * KAS, block_daa_score: 0, covenant_id: None },
        pubkey: keeper(),
    }
}

fn budgets(role: &str) -> kob_protocol::Result<u16> {
    kob_protocol::budget::lookup(role)
}

fn signed(built: &BuiltTx) -> (Transaction, Vec<UtxoEntry>) {
    let sigs = sign_locally(built, &keys()).unwrap();
    let s = finalize(built, &sigs, FinalizeOptions { tighten_budgets: false }).unwrap();
    kob_protocol::verify::validate_signed(&s).unwrap_or_else(|e| panic!("the release validates: {e}"));
    s.tx.to_tx().unwrap()
}

/// Per-input results with the committed budgets enforced.
fn run(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    kob_protocol::verify::execute(tx, entries, true).unwrap()
}

/// Per-input results without the budgets (a changed transaction may need more units for the
/// token program); a transaction the covenant rules refuse as a whole fails every input.
fn loose(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    match kob_protocol::verify::execute(tx, entries, false) {
        Ok(r) => r,
        Err(e) => vec![Err(e.to_string()); tx.inputs.len()],
    }
}

/// A release without a keeper, finalized (nobody signs it).
fn release(l: &Launch) -> (Transaction, Vec<UtxoEntry>, BuiltTx) {
    let built = build_release(&request(l), &budgets).unwrap();
    let (tx, entries) = signed(&built);
    (tx, entries, built)
}

/// Writes the token input's (input `at`) signature script again for `next`, the token outputs'
/// states, after a change of the outputs.
fn relead(tx: &mut Transaction, l: &Launch, at: usize, next: Vec<Kcc20State>) {
    let TokenState::Kcc20(st) = locked_utxo(l).state else { unreachable!() };
    let plan = SigPlan::TokenLeader { template: l.program, state: st, next_states: next, witness: Witness::P2sh(0) };
    tx.inputs[at].signature_script = plan.sigscript(None).unwrap();
}

fn token_spk(l: &Launch, s: &Kcc20State) -> kaspa_consensus_core::tx::ScriptPublicKey {
    TokenState::Kcc20(s.clone()).spk_with(token_template(l.program))
}

fn coin_refuses(tx: &Transaction, entries: &[UtxoEntry], what: &str) {
    let r = loose(tx, entries);
    assert!(r[0].is_err(), "{what}: the lock script took it: {r:?}");
}

const PROGRAMS: [IssueProgram; 2] = [IssueProgram::PublicMint, IssueProgram::Ref3x3];

#[test]
fn the_genesis_declares_an_automatic_lock_and_pays_its_coin() {
    for program in PROGRAMS {
        let l = launch(program);
        let g = &l.plan.genesis;
        let v = l.plan.launch_proof().verify().unwrap();
        assert!(v.json.contains(r#""lock_kind":"auto""#), "{}", v.json);
        assert_eq!(v.token.lock.as_deref(), Some(l.lock.script_hex().as_str()), "the terms, not the script");
        let lock = v.lock.expect("the lock is verified against the genesis");
        assert_eq!(lock.kind, LockKind::Auto);
        assert_eq!((lock.amount, lock.outputs.len(), lock.supply), (LOCKED, 1, Some(FREE + LOCKED)));
        assert_eq!(lock.authority, l.auto.authority());
        assert_eq!(lock.script, l.auto.script());
        assert_eq!(lock.auto(), Some(l.auto));
        assert_eq!(lock.coin.as_ref().map(|c| (c.index, c.amount)), Some((2, AUTO_COIN)));
        assert_eq!(g.tx.outputs[2].script_public_key, l.auto.spk());
        assert!(g.tx.outputs[2].covenant.is_none());
        // read as a manual lock (an older reader), the terms hold nothing
        assert!(verify_genesis_lock(&g.tx, g.covenant_id, &l.lock, LockKind::Manual).is_err());
        // without the coin, nobody could release it: not shown
        let mut tx = g.tx.clone();
        tx.outputs[2].value = 1_000;
        let e = verify_genesis_lock(&tx, g.covenant_id, &l.lock, LockKind::Auto).unwrap_err();
        assert!(e.contains("lock coin"), "{e}");
    }
}

#[test]
fn an_unknown_lock_kind_drops_the_lock() {
    let l = CreatorLock::new(UNTIL, key()).unwrap();
    let obj = kob_protocol::kcc23::parse_object(&format!(
        r#"{{"name":"X","symbol":"XX","decimals":0,"lock":"{}","lock_kind":"soon"}}"#,
        l.script_hex()
    ))
    .unwrap();
    let d = kob_protocol::kcc23::TokenDeclaration::from_object(&obj);
    assert_eq!((d.lock, d.lock_kind), (None, None));
    assert!(d.issues.iter().any(|i| i.starts_with("lock_kind:")), "{:?}", d.issues);
}

#[test]
fn anyone_releases_after_the_time_and_only_the_creator_is_paid() {
    for program in PROGRAMS {
        let l = launch(program);
        let (tx, entries, built) = release(&l);
        assert!(built.sign.is_empty(), "nobody signs a release");
        assert_eq!(built.tx.lock_time, UNTIL);
        assert!(matches!(built.plans[0], SigPlan::CreatorLockAuto { until: UNTIL, .. }));
        assert_ne!(tx.inputs[0].sequence, u64::MAX);
        assert_eq!(tx.outputs.len(), 2);
        assert_eq!(tx.outputs[0].script_public_key, token_spk(&l, &Kcc20State::p2pk(LOCKED as i64, key(), [0; 32])));
        assert_eq!(tx.outputs[0].value, locked_utxo(&l).utxo.amount);
        assert_eq!(tx.outputs[1].script_public_key, p2pk_spk(&key()));
        assert_eq!(tx.outputs[1].value, AUTO_COIN - built.fee.fee);
        assert!(built.fee.fee < MAX_TAKE, "{program:?}: fee {}", built.fee.fee);
        let r = run(&tx, &entries);
        assert!(r.iter().all(|r| r.is_ok()), "{r:?}");
        // the measured budget of the coin is exact: one unit less fails
        let mut tight = tx.clone();
        tight.inputs[0] = kaspa_consensus_core::tx::TransactionInput::new_with_compute_budget(
            tight.inputs[0].previous_outpoint,
            tight.inputs[0].signature_script.clone(),
            tight.inputs[0].sequence,
            auto_budget(l.program) - 1,
        );
        assert!(run(&tight, &entries)[0].is_err(), "{program:?}: {} units", r[0].clone().unwrap());
    }
}

/// The coin's budget of every program a creation may issue: enough, and exact (one unit less fails).
#[test]
fn the_budget_of_every_issued_program() {
    for program in [IssueProgram::PublicMint, IssueProgram::Ref3x3, IssueProgram::KaspaRocketTestnet, IssueProgram::Ref8x8Prototype] {
        let l = launch(program);
        let (tx, entries, _) = release(&l);
        let units = loose(&tx, &entries)[0].clone().unwrap();
        eprintln!("{program:?}: {units} units, budget {}", auto_budget(l.program));
        assert!(run(&tx, &entries).iter().all(|r| r.is_ok()), "{program:?}: {units} units");
        let mut tight = tx.clone();
        tight.inputs[0] = kaspa_consensus_core::tx::TransactionInput::new_with_compute_budget(
            tight.inputs[0].previous_outpoint,
            tight.inputs[0].signature_script.clone(),
            tight.inputs[0].sequence,
            auto_budget(l.program) - 1,
        );
        if matches!(program, IssueProgram::PublicMint | IssueProgram::Ref3x3) {
            assert!(run(&tight, &entries)[0].is_err(), "{program:?}: {units} units, budget not exact");
        }
    }
}

#[test]
fn a_keeper_takes_its_tip_on_its_own_input() {
    for program in PROGRAMS {
        let l = launch(program);
        let mut r = request(&l);
        r.keeper = Some(keeper_coin());
        r.tip = RELEASE_TIP;
        let built = build_release(&r, &budgets).unwrap();
        assert_eq!(built.sign.len(), 1, "the keeper signs its own input only");
        assert_eq!(built.sign[0].pubkey, keeper());
        let (tx, entries) = signed(&built);
        assert_eq!(tx.outputs.len(), 3);
        assert_eq!(tx.outputs[1].script_public_key, p2pk_spk(&key()));
        assert_eq!(tx.outputs[1].value, AUTO_COIN - RELEASE_TIP - built.fee.fee);
        assert_eq!((tx.outputs[2].script_public_key.clone(), tx.outputs[2].value), (p2pk_spk(&keeper()), 5 * KAS + RELEASE_TIP));
        assert!(run(&tx, &entries).iter().all(|r| r.is_ok()));
        // a tip that keeps more than MAX_TAKE with the fee is refused by the builder ...
        r.tip = MAX_TAKE;
        assert!(build_release(&r, &budgets).unwrap_err().to_string().contains("keep"));
        // ... and by the script
        let mut greedy = tx.clone();
        greedy.outputs[1].value = AUTO_COIN - MAX_TAKE - 1;
        greedy.outputs[2].value += MAX_TAKE + 1 - RELEASE_TIP - built.fee.fee;
        coin_refuses(&greedy, &entries, "more than MAX_TAKE kept from the creator");
    }
}

#[test]
fn the_release_is_refused_before_the_time_and_with_a_final_sequence() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries, _) = release(&l);
    for lock_time in [UNTIL - 1, 0, 1_000_000] {
        let mut early = tx.clone();
        early.lock_time = lock_time;
        let r = run(&early, &entries);
        assert!(r[0].as_ref().is_err_and(|e| e.contains("LockTime")), "lock time {lock_time}: {:?}", r[0]);
    }
    let mut fin = tx.clone();
    fin.inputs[0].sequence = u64::MAX;
    assert!(run(&fin, &entries)[0].as_ref().is_err_and(|e| e.contains("finalized")));
    let mut later = tx.clone();
    later.lock_time = UNTIL + 86_400_000;
    assert!(run(&later, &entries).iter().all(|r| r.is_ok()));
}

#[test]
fn the_tokens_go_to_the_creator_whole() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries, _) = release(&l);
    let carrier = tx.outputs[0].value;
    // to another key
    let mut t = tx.clone();
    let thief = Kcc20State::p2pk(LOCKED as i64, keeper(), [0; 32]);
    t.outputs[0].script_public_key = token_spk(&l, &thief);
    relead(&mut t, &l, 1, vec![thief]);
    let r = loose(&t, &entries);
    assert!(r[1].is_ok(), "the token program takes it, the lock coin authorises: {:?}", r[1]);
    coin_refuses(&t, &entries, "tokens to another key");
    // to the creator's key under another owner scheme, or kept under the lock
    for (scheme, owner) in [(0x04u8, key()), (0x03, l.auto.authority())] {
        let mut t = tx.clone();
        let s = Kcc20State { owner, owner_scheme: scheme, ..Kcc20State::p2pk(LOCKED as i64, key(), [0; 32]) };
        t.outputs[0].script_public_key = token_spk(&l, &s);
        relead(&mut t, &l, 1, vec![s]);
        coin_refuses(&t, &entries, &format!("owner scheme {scheme}"));
    }
    // a part to the creator, the rest to another key (the token program conserves the sum)
    let mut t = tx.clone();
    let part = Kcc20State::p2pk(LOCKED as i64 - 1, key(), [0; 32]);
    let rest = Kcc20State::p2pk(1, keeper(), [0; 32]);
    t.outputs[0].script_public_key = token_spk(&l, &part);
    let binding = t.outputs[0].covenant;
    t.outputs.insert(1, TransactionOutput { value: carrier, script_public_key: token_spk(&l, &rest), covenant: binding });
    relead(&mut t, &l, 1, vec![part, rest]);
    let r = loose(&t, &entries);
    assert!(r[1].is_ok(), "the token program takes a split: {:?}", r[1]);
    coin_refuses(&t, &entries, "a part of the tokens");
    // the token output's KAS taken
    let mut t = tx.clone();
    t.outputs[0].value = carrier - 1;
    coin_refuses(&t, &entries, "the token output's KAS taken");
}

#[test]
fn the_kas_goes_back_to_the_creator() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries, built) = release(&l);
    let mut t = tx.clone();
    t.outputs[1].script_public_key = p2pk_spk(&keeper());
    coin_refuses(&t, &entries, "the coin's KAS to another key");
    let mut t = tx.clone();
    t.outputs[1].value = AUTO_COIN - MAX_TAKE - 1;
    coin_refuses(&t, &entries, "more than MAX_TAKE kept");
    let mut t = tx.clone();
    t.outputs[1].value = AUTO_COIN - MAX_TAKE;
    assert!(loose(&t, &entries)[0].is_ok(), "MAX_TAKE exactly");
    // output 1 gone (the creator's KAS left as fee)
    let mut t = tx.clone();
    t.outputs.truncate(1);
    coin_refuses(&t, &entries, "no output 1");
    assert!(built.fee.fee > 0);
}

#[test]
fn only_the_genesis_coin_and_the_genesis_output_go_together() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries, _) = release(&l);
    // a coin sent to the lock's address by another transaction
    let mut t = tx.clone();
    t.inputs[0].previous_outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xc0; 32]), index: 0 };
    coin_refuses(&t, &entries, "a later coin");
    // a token UTXO owned by the lock, made by another transaction (a stray)
    let mut t = tx.clone();
    t.inputs[1].previous_outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xc1; 32]), index: 1 };
    coin_refuses(&t, &entries, "a stray sent to the lock");
    // the creator's free genesis output, released with the coin
    let g = &l.plan.genesis;
    let mut t = tx.clone();
    t.inputs[1].previous_outpoint = TransactionOutpoint { transaction_id: g.txid(), index: 0 };
    let mut e = entries.clone();
    let free = Kcc20State::p2pk(FREE as i64, key(), [0; 32]);
    e[1] = UtxoEntry::new(g.tx.outputs[0].value, token_spk(&l, &free), 0, false, Some(g.covenant_id));
    coin_refuses(&t, &e, "a genesis output the lock does not own");
}

#[test]
fn the_shape_is_fixed() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries, _) = release(&l);
    // the coin at another index: swap the coin and the token input
    let mut t = tx.clone();
    t.inputs.swap(0, 1);
    let mut e = entries.clone();
    e.swap(0, 1);
    let r = loose(&t, &e);
    assert!(r[1].is_err(), "the coin at input 1: {r:?}");
    // a covenant input on the side (a third input with a covenant id)
    let g = &l.plan.genesis;
    let mut t = tx.clone();
    t.inputs.push(t.inputs[1].clone());
    t.inputs[2].previous_outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xc2; 32]), index: 0 };
    let mut e = entries.clone();
    e.push(UtxoEntry::new(KAS, p2pk_spk(&keeper()), 0, false, Some(g.covenant_id)));
    coin_refuses(&t, &e, "a covenant input on the side");
    // four inputs
    let mut t = tx.clone();
    let mut e = entries.clone();
    for k in 0..2u8 {
        let mut i = t.inputs[1].clone();
        i.previous_outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xd0 + k; 32]), index: 0 };
        t.inputs.push(i);
        e.push(UtxoEntry::new(KAS, p2pk_spk(&keeper()), 0, false, None));
    }
    coin_refuses(&t, &e, "four inputs");
    // the coin alone
    let mut t = tx.clone();
    t.inputs.truncate(1);
    coin_refuses(&t, &entries[..1], "no token input");
    // a second output of the token
    let mut t = tx.clone();
    let zero = Kcc20State::p2pk(0, keeper(), [0; 32]);
    let binding = t.outputs[0].covenant;
    t.outputs.push(TransactionOutput { value: KAS, script_public_key: token_spk(&l, &zero), covenant: binding });
    relead(&mut t, &l, 1, vec![Kcc20State::p2pk(LOCKED as i64, key(), [0; 32]), zero]);
    coin_refuses(&t, &entries, "a second token output");
    // an output 0 without the token's covenant id
    let mut t = tx.clone();
    t.outputs[0].covenant = None;
    coin_refuses(&t, &entries, "output 0 not bound to the token");
    let _ = Hash::from_bytes([0; 32]);
}

#[test]
fn the_builder_refuses_what_the_script_would() {
    let l = launch(IssueProgram::PublicMint);
    let mut r = request(&l);
    r.coin.transaction_id = [0xc0; 32];
    assert!(build_release(&r, &budgets).unwrap_err().to_string().contains("same genesis"));
    let mut r = request(&l);
    r.tip = 1;
    assert!(build_release(&r, &budgets).is_err(), "a tip needs a keeper input");
    let mut r = request(&l);
    r.tokens.state = TokenState::Kcc20(Kcc20State::p2pk(LOCKED as i64, key(), [0; 32]));
    assert!(build_release(&r, &budgets).unwrap_err().to_string().contains("not held by this lock"));
    // a lock built for another program's lengths never fits this token
    assert!(!AutoLock::new(l.lock, TemplateId::Kcc20Ref).unwrap().fits(TemplateId::Kcc20PublicMint));
}
