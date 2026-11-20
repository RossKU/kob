//! Tests the creator lock (`kob_protocol::creator_lock`) in the rusty-kaspa v2.1.0 script engine.
//!
//! A genesis locks a share of the supply under an owner of scheme 0x03 whose P2SH redeem script is
//! `<until> OP_CHECKLOCKTIMEVERIFY <key> OP_CHECKSIG`. The unlock spends the locked token UTXO and
//! a KAS coin at the lock's address in one transaction, with lock time `until` and a non-final
//! sequence on the coin. These tests show that only that transaction moves the tokens: an earlier
//! lock time, a final sequence, another key, a missing or fake authority input, and every merge or
//! transfer shape that takes the locked UTXO as a delegate or on the borrow path are refused by the
//! token program or the lock script themselves.

use std::collections::BTreeMap;

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{Transaction, TransactionId, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::TokenRef;
use kob_protocol::creator_lock::{
    build_lock_coin, build_unlock, verify_genesis_lock, CreatorLock, LockCoin, UnlockCreatorLock, LOCK_BUDGET, LOCK_COIN,
};
use kob_protocol::issue::{build_creation, CreationPlan, FundingUtxo, Holder, IssueProgram, IssueSpec};
use kob_protocol::kcc23::TokenDeclaration;
use kob_protocol::state::{Kcc20State, TokenState};
use kob_protocol::tx::{
    finalize, pubkey_of, sighash, sign_digest, sign_locally, BuiltTx, FinalizeOptions, KeyUtxo, SigPlan, TokenUtxo, Utxo, Witness,
};
use secp256k1::SecretKey;

const KAS: u64 = 100_000_000;
const SECRET: [u8; 32] = [0x41; 32];
const OTHER: [u8; 32] = [0x42; 32];
/// 2027-01-01T00:00:00Z
const UNTIL: u64 = 1_798_761_600_000;
const FREE: u64 = 800_000;
const LOCKED: u64 = 200_000;

struct Launch {
    plan: CreationPlan,
    lock: CreatorLock,
    program: TemplateId,
}

fn key() -> [u8; 32] {
    pubkey_of(&SECRET).unwrap()
}

fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    [(pubkey_of(&SECRET).unwrap(), SECRET), (pubkey_of(&OTHER).unwrap(), OTHER)].into_iter().collect()
}

/// A signed creation: output 0 free to the key, output 1 locked until [`UNTIL`].
fn launch(program: IssueProgram) -> Launch {
    let lock = CreatorLock::new(UNTIL, key()).unwrap();
    let funding = FundingUtxo {
        outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([7; 32]), index: 0 },
        amount: 500 * KAS,
        owner_pubkey: key(),
    };
    let holders = vec![Holder::new(key(), 0, FREE), Holder::new(lock.authority(), 0x03, LOCKED)];
    let mut spec = IssueSpec::new("Locked", "LOCK", 2, FREE + LOCKED, holders, vec![funding]);
    spec.program = program;
    spec.lock = Some(lock);
    let mut plan = build_creation(&spec).unwrap();
    plan.sign(&SecretKey::from_slice(&SECRET).unwrap()).unwrap();
    plan.verify().unwrap();
    Launch { plan, lock, program: program.template_id() }
}

fn genesis_utxo(l: &Launch, at: usize) -> TokenUtxo {
    let g = &l.plan.genesis;
    let s = &g.states[at];
    TokenUtxo {
        utxo: Utxo {
            transaction_id: g.txid().as_bytes(),
            index: at as u32,
            amount: g.tx.outputs[at].value,
            block_daa_score: 0,
            covenant_id: Some(g.covenant_id.as_bytes()),
        },
        state: TokenState::Kcc20(Kcc20State {
            amount: s.amount as i64,
            owner: s.owner,
            owner_scheme: s.owner_scheme,
            borrow_scheme: s.borrow_scheme,
            borrow_guard: s.borrow_guard,
            extension_commitment: s.extension_commitment,
        }),
    }
}

fn coin(amount: u64) -> Utxo {
    Utxo { transaction_id: [0xc0; 32], index: 0, amount, block_daa_score: 0, covenant_id: None }
}

fn unlock_request(l: &Launch) -> UnlockCreatorLock {
    UnlockCreatorLock {
        token: TokenRef { covenant_id: l.plan.genesis.covenant_id.as_bytes(), program: l.program },
        lock: l.lock,
        tokens: vec![genesis_utxo(l, 1)],
        coin: coin(LOCK_COIN),
        to: None,
        carrier: None,
        funding: vec![],
        change: None,
        fee: Default::default(),
    }
}

fn budgets(role: &str) -> kob_protocol::Result<u16> {
    kob_protocol::budget::lookup(role)
}

fn signed(built: &BuiltTx) -> (Transaction, Vec<UtxoEntry>) {
    let sigs = sign_locally(built, &keys()).unwrap();
    let s = finalize(built, &sigs, FinalizeOptions { tighten_budgets: false }).unwrap();
    kob_protocol::verify::validate_signed(&s).unwrap_or_else(|e| panic!("the unlock validates: {e}"));
    s.tx.to_tx().unwrap()
}

/// Per-input results with the committed budgets enforced.
fn run(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    kob_protocol::verify::execute(tx, entries, true).unwrap()
}

/// Signs input `at` (a lock coin) again with `secret` after a change of the transaction.
fn resign_coin(tx: &mut Transaction, entries: &[UtxoEntry], at: usize, lock: &CreatorLock, secret: &[u8; 32]) {
    let sig = sign_digest(secret, &sighash(tx, entries, at)).unwrap();
    tx.inputs[at].signature_script = lock.sig_script(&sig);
}

#[test]
fn the_genesis_declares_the_lock_and_a_reader_verifies_what_it_holds() {
    for program in [IssueProgram::PublicMint, IssueProgram::Ref3x3] {
        let l = launch(program);
        let v = l.plan.launch_proof().verify().unwrap();
        assert_eq!(v.token.lock.as_deref(), Some(l.lock.script_hex().as_str()));
        let lock = v.lock.expect("the lock is verified against the genesis");
        assert_eq!(
            (lock.amount, lock.outputs.iter().map(|o| (o.index, o.amount)).collect::<Vec<_>>(), lock.supply, lock.until),
            (LOCKED, vec![(1, LOCKED)], Some(FREE + LOCKED), UNTIL)
        );
        assert_eq!(lock.program, l.program);
        assert_eq!(lock.states(), vec![(1, l.lock.state(LOCKED as i64, [0; 32]))]);
        assert_eq!(lock.authority, l.plan.genesis.states[1].owner);
        // another lock (one millisecond later, or another key) holds nothing in this genesis
        let g = &l.plan.genesis;
        let later = CreatorLock::new(UNTIL + 1, key()).unwrap();
        assert!(verify_genesis_lock(&g.tx, g.covenant_id, &later).is_err());
        let other = CreatorLock::new(UNTIL, pubkey_of(&OTHER).unwrap()).unwrap();
        assert!(verify_genesis_lock(&g.tx, g.covenant_id, &other).is_err());
        assert!(verify_genesis_lock(&g.tx, Hash::from_bytes([9; 32]), &l.lock).is_err());
    }
}

/// A genesis served with an extra output that names the token's id under another authorizing
/// input, with its state in the payload. The id still checks, because it commits only to the
/// group of its own input, but the extra output is not the token's: it never adds to the locked
/// amount or the supply. Named on top of the genesis's own states, it is one state too many and the
/// lock is not shown; named in place of one, that state is the only one not proven.
#[test]
fn an_output_the_token_id_does_not_bind_is_not_counted() {
    use kaspa_consensus_core::tx::CovenantBinding;
    for program in [IssueProgram::PublicMint, IssueProgram::Ref3x3] {
        let l = launch(program);
        let g = &l.plan.genesis;
        let mut tx = g.tx.clone();
        let extra = TokenState::Kcc20(l.lock.state(5_000_000, [0; 32]));
        let spk = extra.spk_with(kob_protocol::artifacts::token_template(l.program));
        tx.inputs.push(TransactionInput::new_with_compute_budget(
            TransactionOutpoint { transaction_id: TransactionId::from_bytes([0xee; 32]), index: 0 },
            vec![],
            0,
            0,
        ));
        tx.outputs.push(TransactionOutput::with_covenant(tx.outputs[1].value, spk, Some(CovenantBinding::new(1, g.covenant_id))));
        let own = kob_protocol::genesis_states::decode(&tx.payload).unwrap();
        // on top of the genesis's own states: refused, so no made-up figure is shown
        let mut more = own.clone();
        more.push(extra.encode());
        tx.payload = kob_protocol::genesis_states::encode(&more).unwrap();
        assert!(verify_genesis_lock(&tx, g.covenant_id, &l.lock).is_err());
        let v = kob_protocol::kcc23::LaunchProof::of(&l.plan.prep.tx, &tx, g.covenant_id).verify().unwrap();
        assert!(v.lock.is_none() && v.token.issues.iter().any(|i| i.starts_with("lock:")), "{:?}", v.token.issues);
        // in place of the free holder's state: only the bound lock counts, and the supply is unknown
        let mut swapped = own.clone();
        swapped[0] = extra.encode();
        tx.payload = kob_protocol::genesis_states::encode(&swapped).unwrap();
        let lock = verify_genesis_lock(&tx, g.covenant_id, &l.lock).unwrap();
        assert_eq!((lock.amount, lock.outputs.iter().map(|o| o.index).collect::<Vec<_>>(), lock.supply), (LOCKED, vec![1], None));
        let v = kob_protocol::kcc23::LaunchProof::of(&l.plan.prep.tx, &tx, g.covenant_id).verify().unwrap();
        assert_eq!(v.lock.map(|x| (x.amount, x.supply)), Some((LOCKED, None)));
    }
}

#[test]
fn a_look_alike_script_is_not_a_lock() {
    let l = CreatorLock::new(UNTIL, key()).unwrap();
    let s = l.script();
    let mut checksigverify = s.clone();
    checksigverify[43] = 0xad;
    let mut daa = s.clone();
    daa[1..9].copy_from_slice(&5_000_000u64.to_le_bytes());
    for bad in [hex(&checksigverify), hex(&daa), hex(&s[..43]), format!("{}75", hex(&s))] {
        let obj = kob_protocol::kcc23::parse_object(&format!(r#"{{"name":"X","symbol":"XX","decimals":0,"lock":"{bad}"}}"#)).unwrap();
        let d = TokenDeclaration::from_object(&obj);
        assert_eq!(d.lock, None, "{bad}");
        assert!(d.issues.iter().any(|i| i.starts_with("lock:")), "{:?}", d.issues);
    }
    // a declared lock that owns no holder is refused at issuance
    let funding = FundingUtxo {
        outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes([7; 32]), index: 0 },
        amount: 500 * KAS,
        owner_pubkey: key(),
    };
    let mut spec = IssueSpec::new("Locked", "LOCK", 2, 10, vec![Holder::new(key(), 0, 10)], vec![funding]);
    spec.lock = Some(l);
    assert!(build_creation(&spec).unwrap_err().to_string().contains("no holder is owned by the lock"));
}

#[test]
fn the_unlock_is_accepted_at_the_lock_time_with_the_key() {
    for program in [IssueProgram::PublicMint, IssueProgram::Ref3x3] {
        let l = launch(program);
        let built = build_unlock(&unlock_request(&l), &budgets).unwrap();
        assert_eq!(built.tx.lock_time, UNTIL);
        assert!(matches!(built.plans[0], SigPlan::CreatorLock { until: UNTIL, .. }));
        assert!(matches!(&built.plans[1], SigPlan::TokenLeader { witness: Witness::P2sh(0), .. }));
        assert_eq!(built.sign.len(), 1, "only the coin is signed");
        assert_eq!(built.sign[0].redeem_script.as_deref(), Some(l.lock.script().as_slice()));
        let (tx, entries) = signed(&built);
        assert_ne!(tx.inputs[0].sequence, u64::MAX, "the coin's sequence is not final");
        // the tokens go to the key as a plain holder, and the coin's rest comes back as change
        let out = &built.tx.outputs;
        let SigPlan::TokenLeader { next_states, .. } = &built.plans[1] else { unreachable!() };
        assert_eq!(next_states, &vec![Kcc20State::p2pk(LOCKED as i64, key(), [0; 32])]);
        assert_eq!(out.len(), 2);
        assert!(run(&tx, &entries).iter().all(|r| r.is_ok()));
        // the measured budget of the coin is exact: one unit less fails
        let mut tight = tx.clone();
        tight.inputs[0] = TransactionInput::new_with_compute_budget(
            tight.inputs[0].previous_outpoint,
            tight.inputs[0].signature_script.clone(),
            tight.inputs[0].sequence,
            LOCK_BUDGET - 1,
        );
        assert!(run(&tight, &entries)[0].is_err(), "{program:?}: {} units", run(&tx, &entries)[0].clone().unwrap());
    }
}

#[test]
fn the_unlock_is_refused_before_the_lock_time_and_with_a_final_sequence() {
    let l = launch(IssueProgram::PublicMint);
    let built = build_unlock(&unlock_request(&l), &budgets).unwrap();
    let (tx, entries) = signed(&built);
    // one millisecond early, and a lock time of 0 or a DAA score: CHECKLOCKTIMEVERIFY refuses
    for lock_time in [UNTIL - 1, 0, 1_000_000] {
        let mut early = tx.clone();
        early.lock_time = lock_time;
        resign_coin(&mut early, &entries, 0, &l.lock, &SECRET);
        let r = run(&early, &entries);
        assert!(r[0].as_ref().is_err_and(|e| e.contains("LockTime")), "lock time {lock_time}: {:?}", r[0]);
    }
    // a final sequence on the coin would bypass the lock time at the transaction level: refused
    let mut fin = tx.clone();
    fin.inputs[0].sequence = u64::MAX;
    resign_coin(&mut fin, &entries, 0, &l.lock, &SECRET);
    assert!(run(&fin, &entries)[0].as_ref().is_err_and(|e| e.contains("finalized")));
    // a later lock time is fine for the script (the node takes it once its median time passes it)
    let mut later = tx.clone();
    later.lock_time = UNTIL + 86_400_000;
    resign_coin(&mut later, &entries, 0, &l.lock, &SECRET);
    assert!(run(&later, &entries).iter().all(|r| r.is_ok()));
}

#[test]
fn another_key_cannot_unlock() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries) = signed(&build_unlock(&unlock_request(&l), &budgets).unwrap());
    let mut wrong = tx.clone();
    resign_coin(&mut wrong, &entries, 0, &l.lock, &OTHER);
    assert!(run(&wrong, &entries)[0].is_err());
    // nor a lock of another key: its coin is another address, which the token program refuses
    let thief = CreatorLock::new(UNTIL, pubkey_of(&OTHER).unwrap()).unwrap();
    let mut entries2 = entries.clone();
    entries2[0] = UtxoEntry::new(LOCK_COIN, thief.spk(), 0, false, None);
    let mut t = tx.clone();
    resign_coin(&mut t, &entries2, 0, &thief, &OTHER);
    let r = run(&t, &entries2);
    assert!(r[0].is_ok(), "the thief's own coin is theirs");
    assert!(r[1].is_err(), "but the token program wants this lock's coin");
}

#[test]
fn the_tokens_do_not_move_without_the_lock_coin() {
    let l = launch(IssueProgram::PublicMint);
    let (tx, entries) = signed(&build_unlock(&unlock_request(&l), &budgets).unwrap());
    // the witness names input 0, but input 0 is a P2PK coin of the key
    let mut entries2 = entries.clone();
    entries2[0] = UtxoEntry::new(LOCK_COIN, kob_protocol::script::p2pk_spk(&key()), 0, false, None);
    let mut t = tx.clone();
    let sig = sign_digest(&SECRET, &sighash(&t, &entries2, 0)).unwrap();
    t.inputs[0].signature_script = kob_protocol::script::push_data(&sig);
    let r = run(&t, &entries2);
    assert!(r[0].is_ok() && r[1].is_err(), "{r:?}");
    // the token input alone, its witness naming itself or an input that does not exist
    let g = &l.plan.genesis;
    let locked = genesis_utxo(&l, 1);
    let next = vec![Kcc20State::p2pk(LOCKED as i64, key(), [0; 32])];
    for (witness, sig) in [
        (Witness::P2sh(0), None),
        (Witness::P2sh(5), None),
        (Witness::P2pk(key()), Some(true)),
        (Witness::Borrow, None),
        (Witness::CovenantId, None),
    ] {
        let TokenState::Kcc20(st) = &locked.state else { unreachable!() };
        let plan =
            SigPlan::TokenLeader { template: l.program, state: st.clone(), next_states: next.clone(), witness: witness.clone() };
        let (mut tx, entries) = single_token_tx(g, &locked, &plan, &next);
        let s = sig.map(|_| sign_digest(&SECRET, &sighash(&tx, &entries, 0)).unwrap());
        tx.inputs[0].signature_script = plan.sigscript(s.as_deref()).unwrap();
        assert!(run(&tx, &entries)[0].is_err(), "{witness:?}");
    }
}

/// A transaction spending only `tok` with `plan`, paying `next` to the key, plus an OP_TRUE fee input.
fn single_token_tx(
    g: &kob_protocol::issue::GenesisPlan,
    tok: &TokenUtxo,
    plan: &SigPlan,
    next: &[Kcc20State],
) -> (Transaction, Vec<UtxoEntry>) {
    let program = kob_protocol::artifacts::token_template(g.spec.program.template_id());
    let inputs = vec![TransactionInput::new_with_compute_budget(tok.utxo.outpoint(), vec![], 0, 500)];
    let entries = vec![entry(&tok.utxo, plan.spk())];
    let outputs = next
        .iter()
        .map(|s| TransactionOutput {
            value: tok.utxo.amount,
            script_public_key: TokenState::Kcc20(s.clone()).spk_with(program),
            covenant: Some(kaspa_consensus_core::tx::CovenantBinding { authorizing_input: 0, covenant_id: g.covenant_id }),
        })
        .collect();
    let tx = Transaction::new(1, inputs, outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
    (tx, entries)
}

#[test]
fn a_merge_or_transfer_cannot_carry_the_locked_utxo_along() {
    let l = launch(IssueProgram::PublicMint);
    let g = &l.plan.genesis;
    let free = genesis_utxo(&l, 0);
    let locked = genesis_utxo(&l, 1);
    let (TokenState::Kcc20(fs), TokenState::Kcc20(ls)) = (&free.state, &locked.state) else { unreachable!() };
    let program = kob_protocol::artifacts::token_template(l.program);
    // a merge led by the key's free UTXO (signed by the key) with the locked UTXO as a delegate
    let merged = vec![Kcc20State::p2pk((FREE + LOCKED) as i64, key(), [0; 32])];
    for delegate_witness in [Witness::P2pk(key()), Witness::P2sh(0), Witness::P2sh(1), Witness::CovenantId] {
        let leader = SigPlan::TokenLeader {
            template: l.program,
            state: fs.clone(),
            next_states: merged.clone(),
            witness: Witness::P2pk(key()),
        };
        let delegate = SigPlan::TokenDelegator { template: l.program, state: ls.clone(), witness: delegate_witness.clone() };
        let inputs = vec![
            TransactionInput::new_with_compute_budget(free.utxo.outpoint(), vec![], 0, 500),
            TransactionInput::new_with_compute_budget(locked.utxo.outpoint(), vec![], 0, 500),
        ];
        let entries = vec![entry(&free.utxo, leader.spk()), entry(&locked.utxo, delegate.spk())];
        let outputs = vec![TransactionOutput {
            value: free.utxo.amount + locked.utxo.amount,
            script_public_key: TokenState::Kcc20(merged[0].clone()).spk_with(program),
            covenant: Some(kaspa_consensus_core::tx::CovenantBinding { authorizing_input: 0, covenant_id: g.covenant_id }),
        }];
        let mut tx = Transaction::new(1, inputs, outputs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
        let s0 = sign_digest(&SECRET, &sighash(&tx, &entries, 0)).unwrap();
        let s1 = sign_digest(&SECRET, &sighash(&tx, &entries, 1)).unwrap();
        tx.inputs[0].signature_script = leader.sigscript(Some(&s0)).unwrap();
        tx.inputs[1].signature_script = delegate.sigscript(Some(&s1)).unwrap();
        let r = run(&tx, &entries);
        assert!(r[0].is_ok(), "the key's own leader is authorised: {:?}", r[0]);
        assert!(r[1].is_err(), "the locked delegate is not ({delegate_witness:?})");
    }
    // a transfer of the free UTXO that pays into the lock is a deposit, never a withdrawal: the
    // program conserves the amounts, so the locked UTXO cannot be split off by its outputs either
    let st = TokenState::Kcc20(ls.clone());
    assert!(!st.is_user(), "the builders do not take a P2SH-owned state as the wallet's");
    let send = kob_protocol::build::SendTokens {
        token: TokenRef { covenant_id: g.covenant_id.as_bytes(), program: l.program },
        tokens: vec![locked.clone()],
        recipients: vec![kob_protocol::build::TokenRecipient { pubkey: key(), amount: LOCKED as i64, carrier: 10 * KAS }],
        token_change: None,
        token_change_carrier: 0,
        funding: vec![],
        change: Some(key()),
        records: vec![],
        fee: Default::default(),
    };
    assert!(kob_protocol::build::build_send_tokens(&send, &budgets).is_err(), "kob send / merge refuse a locked UTXO");
}

#[test]
fn the_lock_coin_and_a_coin_short_of_the_fee() {
    let l = launch(IssueProgram::PublicMint);
    let funding = KeyUtxo {
        utxo: Utxo { transaction_id: [3; 32], index: 1, amount: 50 * KAS, block_daa_score: 0, covenant_id: None },
        pubkey: key(),
    };
    let c = build_lock_coin(
        &LockCoin { lock: l.lock, funding: vec![funding.clone()], amount: None, change: None, fee: Default::default() },
        &budgets,
    )
    .unwrap();
    assert_eq!(c.tx.outputs[0].value, LOCK_COIN);
    assert_eq!(c.tx.outputs[0].script_public_key, kob_protocol::tx::spk_to_string(&l.lock.spk()));
    signed(&c);
    // the unlock spends the coin the lock-coin transaction made
    let mut r = unlock_request(&l);
    r.coin = Utxo { transaction_id: c.tx.id, index: 0, amount: LOCK_COIN, block_daa_score: 0, covenant_id: None };
    signed(&build_unlock(&r, &budgets).unwrap());
    // a coin too small for the fee needs a funding input of the key
    r.coin.amount = 1_000;
    assert!(build_unlock(&r, &budgets).is_err());
    r.funding = vec![funding];
    signed(&build_unlock(&r, &budgets).unwrap());
    // a token UTXO the lock does not hold is refused by the builder
    let mut r = unlock_request(&l);
    r.tokens = vec![genesis_utxo(&l, 0)];
    assert!(build_unlock(&r, &budgets).unwrap_err().to_string().contains("not held by this lock"));
}

fn entry(u: &Utxo, spk: kaspa_consensus_core::tx::ScriptPublicKey) -> UtxoEntry {
    UtxoEntry::new(u.amount, spk, u.block_daa_score, false, u.covenant_id.map(Hash::from_bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
