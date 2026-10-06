//! Review B2 (internal listing review): regression tests for the two pinned KRON token programs
//! (`contracts/adapters/kron/templates/kron_token_{2433,2732}.bin`, real bytes) and for the KOB-side mitigations
//! the listing verdict relies on. Engine: rusty-kaspa v2.1.0 `TxScriptEngine` with the KIP-20 covenant context.
//! Every engine test runs on BOTH templates and asserts the observed outcome: `ACCEPTED` = the engine let the
//! transaction through, `REJECTED` = it did not. The review report itself is not part of this repository.
//!
//! Program as reconstructed in the review (both templates, one entry, every covenant token input runs all of it):
//!   n_out = len(owners)/32 in 1..=5; n_in = OpCovInputCount(own cov) in 1..=4; OpCovOutputCount == n_out
//!   for i < n_in: state_i = last 2433/2732 bytes of the sigscript of covenant input i; auth by state_i.type:
//!       0 checkSig(sigs[65i..], owner)  1 inSpk(wits[i]) == P2SH(owner)  2 inCovId(wits[i]) == owner
//!       3 inSpk(wits[i]) == P2PK(owner)  else fail;  amount_i >= 0;  sum_in += amount_i
//!   for j < n_out: (amount_j >= 1 || (minter_j && amount_j == 0)); amount_j <= 1e9;
//!       outSpk(covOutIdx(j)) == P2SH(state_j || own suffix)
//!   if !self.is_minter: sum_in == sum_out and no output minter flag (a minter input skips both checks)
//!   2732 only: witness index in [0, nIns), type-2 owner != 0^32, input amount <= 1e9, output type in 0..=3.
//!
//! Differences to an earlier unrun draft of these tests: token outputs are bound to a TOKEN input (a binding to a
//! non-covenant input is a genesis and fails the covenant context), SINGLE|ANYONECANPAY is 0x84 (0x83 is not a valid
//! Kaspa sighash type), plus the genesis-check, signer and order-genesis tests.
//!
//! Run: cargo test -p kob-tests --test review_b2_kron -- --nocapture --test-threads=1
#![allow(clippy::needless_range_loop)]

mod common;

use kaspa_consensus_core::hashing::covenant_id::covenant_id;
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
use kaspa_txscript_errors::TxScriptError;
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};

use kob_protocol::registry::{verify_genesis, GenesisError, GenesisOutput, Registry, DEFAULT_REGISTRY_JSON};

use common::kron::{kron_ss, ks, Kron, KS, TPL_2433, TPL_2732, T_ADDR, T_COVID, T_PUBKEY};
use common::push_redeem_script;

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const ORDER_COV: Hash = Hash::from_bytes([0xc0; 32]);
const OTHER_COV: Hash = Hash::from_bytes([0xc1; 32]);
const SIGOP_SCRIPT_UNITS: u64 = 100_000;

fn keypair() -> Keypair {
    let secp = Secp256k1::new();
    let mut rng = thread_rng();
    let mut sk = [0u8; 32];
    loop {
        rng.fill_bytes(&mut sk);
        if let Ok(s) = SecretKey::from_slice(&sk) {
            return Keypair::from_secret_key(&secp, &s);
        }
    }
}
fn pk(kp: &Keypair) -> [u8; 32] {
    kp.x_only_public_key().0.serialize()
}
fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn p2pk_spk(pk: &[u8; 32]) -> ScriptPublicKey {
    let mut s = vec![OpData32];
    s.extend_from_slice(pk);
    s.push(OpCheckSig);
    ScriptPublicKey::new(0, s.into())
}
fn minter(owner: [u8; 32], typ: u8, amount: i64) -> KS {
    KS { owner, typ, amount, minter: 1 }
}

fn templates() -> Vec<(&'static str, Kron)> {
    vec![("kron-2433", Kron::load(TPL_2433)), ("kron-2732", Kron::load(TPL_2732))]
}

// ---------------------------------------------------------------- tx model

#[derive(Clone)]
enum Role {
    /// KRON token input in state `s`; `next` / `wit` override the shared columns for this input only
    Tok { s: KS, next: Option<Vec<KS>>, wit: Option<Vec<u8>> },
    /// P2PK input of `kp` signed with sighash type `ht`
    P2pk { kp: Keypair, ht: u8 },
    /// OpTrue-P2SH input carrying covenant id `cov` (stands in for any input of that covenant, e.g. a KOB order)
    Cov,
    /// OpTrue-P2SH input without covenant
    Plain,
}
#[derive(Clone)]
struct In {
    cov: Option<Hash>,
    role: Role,
}
struct Tx {
    ins: Vec<In>,
    outs: Vec<TransactionOutput>,
    /// shared next-state columns and witness column (one byte per covenant token input, in covenant order)
    next: Vec<KS>,
    wit: Vec<u8>,
    /// type-0 signers per covenant position (None: 65 zero bytes)
    sig_keys: Vec<Option<Keypair>>,
}

fn tok(s: KS) -> In {
    In { cov: Some(TOKEN_COV), role: Role::Tok { s, next: None, wit: None } }
}
fn p2pk(kp: &Keypair) -> In {
    In { cov: None, role: Role::P2pk { kp: *kp, ht: 0x01 } }
}
fn p2pk_ht(kp: &Keypair, ht: u8) -> In {
    In { cov: None, role: Role::P2pk { kp: *kp, ht } }
}
fn cov_in(c: Hash) -> In {
    In { cov: Some(c), role: Role::Cov }
}
fn plain() -> In {
    In { cov: None, role: Role::Plain }
}
/// Token outputs bound to the token covenant with authorising input `auth` (must be a token input: a binding to an
/// input without the token covenant id is a genesis and fails the covenant context).
fn tok_outs(k: &Kron, next: &[KS], auth: u16) -> Vec<TransactionOutput> {
    next.iter()
        .map(|s| TransactionOutput {
            value: KAS as u64,
            script_public_key: k.spk(s),
            covenant: Some(CovenantBinding { authorizing_input: auth, covenant_id: TOKEN_COV }),
        })
        .collect()
}
fn role_name(r: &Role) -> &'static str {
    match r {
        Role::Tok { .. } => "kron.token",
        Role::P2pk { .. } => "p2pk",
        Role::Cov => "cov-presence",
        Role::Plain => "plain",
    }
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair, ht: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(ht).expect("valid sighash type"), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(ht);
    s
}

fn build(k: &Kron, t: &Tx) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = t
        .ins
        .iter()
        .map(|i| {
            let spk = match &i.role {
                Role::Tok { s, .. } => k.spk(s),
                Role::P2pk { kp, .. } => p2pk_spk(&pk(kp)),
                Role::Cov | Role::Plain => pay_to_script_hash_script(&[OpTrue]),
            };
            UtxoEntry::new((10 * KAS) as u64, spk, 0, false, i.cov)
        })
        .collect();
    let inputs: Vec<TransactionInput> = (0..t.ins.len())
        .map(|n| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([n as u8 + 1; 32]), index: n as u32 },
                vec![],
                0,
                0,
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, t.outs.clone(), 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (n, i) in t.ins.iter().enumerate() {
        tx.inputs[n].signature_script = match &i.role {
            Role::Tok { s, next, wit } => {
                // type-0 signatures are checked against the EXECUTING input's sighash, so every token input carries
                // its own signature column
                let mut sigs = vec![];
                for key in &t.sig_keys {
                    match key {
                        Some(kp) => sigs.extend(sign(&unsigned, &entries, n, kp, 0x01)),
                        None => sigs.extend([0u8; 65]),
                    }
                }
                kron_ss(&k.redeem(s), next.as_ref().unwrap_or(&t.next), &sigs, wit.as_ref().unwrap_or(&t.wit))
            }
            Role::P2pk { kp, ht } => push_redeem_script(&sign(&unsigned, &entries, n, kp, *ht)),
            Role::Cov | Role::Plain => push_redeem_script(&[OpTrue]),
        };
    }
    (tx, entries)
}

fn execute(tx: &Transaction, entries: &[UtxoEntry]) -> Result<Vec<Result<(), TxScriptError>>, String> {
    let populated = PopulatedTransaction::new(tx, entries.to_vec());
    let cov_ctx = CovenantsContext::from_tx(&populated).map_err(|e| format!("covenant context: {e:?}"))?;
    let reused = SigHashReusedValuesUnsync::new();
    let cache = Cache::new(10_000);
    Ok((0..tx.inputs.len())
        .map(|i| {
            let input = tx.inputs[i].clone();
            let mut vm = TxScriptEngine::from_transaction_input(
                &populated,
                &input,
                i,
                &entries[i],
                EngineCtx::new(&cache).with_reused(&reused).with_covenants_ctx(&cov_ctx),
                EngineFlags { sigop_script_units: SIGOP_SCRIPT_UNITS.into() },
            );
            vm.execute()
        })
        .collect())
}

/// True when every input passes; prints the per-input outcome.
fn run(tpl: &str, k: &Kron, name: &str, t: &Tx) -> bool {
    let (tx, entries) = build(k, t);
    match execute(&tx, &entries) {
        Err(e) => {
            println!("B2 {tpl} {name}: REJECTED ({e})");
            false
        }
        Ok(res) => {
            let mut why = String::new();
            for (n, r) in res.iter().enumerate() {
                if let Err(e) = r {
                    why.push_str(&format!(" in[{n}] {}: {e:?};", role_name(&t.ins[n].role)));
                }
            }
            let ok = why.is_empty();
            println!("B2 {tpl} {name}: {}{}", if ok { "ACCEPTED" } else { "REJECTED" }, why);
            ok
        }
    }
}

// ---------------------------------------------------------------- program pins and declared capabilities

#[test]
fn r_kr_00_pinned_bytes_match_the_registry_entry() {
    let reg = Registry::parse(DEFAULT_REGISTRY_JSON).expect("registry");
    for (id, k) in templates() {
        let t = reg.templates.iter().find(|t| t.id == id).expect("template");
        assert_eq!(hexs(&k.hash), t.template_hash, "{id}: template hash");
        assert_eq!(k.suffix.len() as u32, t.suffix_len, "{id}: suffix length");
        assert_eq!((t.prefix_len, t.state_len), (0, 46), "{id}: prefix / state length");
        assert_eq!((t.max_token_inputs, t.max_token_outputs), (4, 5), "{id}: slot limits");
        assert_eq!((t.escrow.id_type, t.escrow.is_minter, t.escrow.delivery_id_type), (Some(2), Some(0), Some(3)), "{id}: escrow");
        let caps: Vec<&str> = t.capabilities.iter().map(|c| c.as_str()).collect();
        assert!(caps.contains(&"mint-authority"), "{id}: the is_minter branch must be declared: {caps:?}");
        for none in ["freeze", "seize", "blacklist"] {
            assert!(!caps.contains(&none), "{id}: the program has no {none} path");
        }
        println!("B2 {id} template {} suffix {} capabilities {caps:?}", hexs(&k.hash), k.suffix.len());
    }
}

// ---------------------------------------------------------------- conservation and supply

#[test]
fn r_kr_01_holder_conservation_and_output_bounds() {
    for (tpl, k) in templates() {
        let a = keypair();
        let t = |next: Vec<KS>| Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 1000)), p2pk(&a)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "baseline type-3 1->2", &t(vec![ks([0x41; 32], T_ADDR, 600), ks(pk(&a), T_ADDR, 400)])));
        assert!(!run(tpl, &k, "holder inflates by 1", &t(vec![ks([0x41; 32], T_ADDR, 601), ks(pk(&a), T_ADDR, 400)])));
        assert!(!run(tpl, &k, "holder burns 1 (out < in)", &t(vec![ks([0x41; 32], T_ADDR, 999)])));
        assert!(!run(
            tpl,
            &k,
            "non-minter creates an is_minter=1 output",
            &t(vec![ks([0x41; 32], T_ADDR, 999), minter([0x42; 32], T_ADDR, 1)])
        ));
        let mut m2 = ks([0x42; 32], T_ADDR, 1);
        m2.minter = 2;
        assert!(!run(tpl, &k, "non-minter creates an is_minter=2 output", &t(vec![ks([0x41; 32], T_ADDR, 999), m2])));
        assert!(!run(
            tpl,
            &k,
            "non-minter creates a zero-amount output",
            &t(vec![ks([0x41; 32], T_ADDR, 1000), ks([0x42; 32], T_ADDR, 0)])
        ));
        assert!(!run(tpl, &k, "negative output amount", &t(vec![ks([0x41; 32], T_ADDR, 1001), ks([0x42; 32], T_ADDR, -1)])));
        // an output above 1e9 (two inputs summing to 1e9 + 1)
        let b = keypair();
        let next = vec![ks([0x41; 32], T_ADDR, 1_000_000_001)];
        let t2 = Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 600_000_000)), tok(ks(pk(&b), T_ADDR, 400_000_001)), p2pk(&a), p2pk(&b)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 3],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "output amount 1e9+1", &t2));
    }
}

#[test]
fn r_kr_02_minter_can_mint_without_limit() {
    // an input whose own is_minter flag is set skips conservation and may create more minters (capability
    // mint-authority). Tokens without a live minter UTXO cannot regain one (a non-minter input refuses minter outputs).
    for (tpl, k) in templates() {
        let m = keypair();
        let next = vec![
            ks([0x41; 32], T_ADDR, 1_000_000_000),
            ks([0x42; 32], T_ADDR, 1_000_000_000),
            minter(pk(&m), T_ADDR, 0),
            minter([0x43; 32], T_ADDR, 0),
        ];
        let t = Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), p2pk(&m)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "minter (0 balance) mints 2e9 and clones itself", &t));
        // ... but it still needs its owner's authorisation
        let thief = keypair();
        let next = vec![ks(pk(&thief), T_ADDR, 1_000_000_000)];
        let t = Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), p2pk(&thief)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "minter used without its owner", &t));
    }
}

#[test]
fn r_kr_03_minting_transaction_cannot_touch_an_escrow() {
    for (tpl, k) in templates() {
        let m = keypair();
        let esc = ks(ORDER_COV.as_bytes(), T_COVID, 1000);
        // minter + escrow WITH the order present, extra minted: the escrow (non-minter) input enforces conservation
        let next = vec![ks([0x66; 32], T_ADDR, 5000)];
        let t = Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), tok(esc), p2pk(&m), cov_in(ORDER_COV)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 3],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "minter + escrow (order present) mints extra", &t));
        // minter + escrow WITHOUT the order: every token input authorises every covenant input
        let next = vec![ks([0x66; 32], T_ADDR, 1000)];
        let t = Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), tok(esc), p2pk(&m)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 2],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "minter moves an escrow without the order", &t));
    }
}

// ---------------------------------------------------------------- escrow (id_type 2) authorisation

#[test]
fn r_kr_04_escrow_needs_an_input_of_the_order_covenant() {
    for (tpl, k) in templates() {
        let esc = ks(ORDER_COV.as_bytes(), T_COVID, 1000);
        let next = vec![ks([0x66; 32], T_ADDR, 1000)];
        let mk = |ins: Vec<In>, wit: Vec<u8>| Tx { ins, outs: tok_outs(&k, &next, 0), next: next.clone(), wit, sig_keys: vec![] };
        assert!(!run(tpl, &k, "escrow, witness -> the token input itself", &mk(vec![tok(esc), plain()], vec![0])));
        assert!(!run(tpl, &k, "escrow, witness -> a plain input", &mk(vec![tok(esc), plain()], vec![1])));
        assert!(!run(tpl, &k, "escrow, witness out of range", &mk(vec![tok(esc), plain()], vec![9])));
        assert!(!run(tpl, &k, "escrow, witness -> an input of ANOTHER covenant", &mk(vec![tok(esc), cov_in(OTHER_COV)], vec![1])));
        let thief = keypair();
        assert!(!run(tpl, &k, "escrow, witness -> the thief's P2PK input", &mk(vec![tok(esc), p2pk(&thief)], vec![1])));
        // DEPENDENCY: ANY input carrying the order covenant id releases the escrow; the token program does not
        // look at what that input is. The order covenant (and the uniqueness of its covenant id, see r_kr_15) is the
        // only guard.
        assert!(run(
            tpl,
            &k,
            "escrow + any input of the order covenant (dependency)",
            &mk(vec![tok(esc), cov_in(ORDER_COV)], vec![1])
        ));
    }
}

#[test]
fn r_kr_05_zero_owner_id_type_2_is_anyone_can_spend_on_2433_only() {
    // (a) OpInputCovenantId of an input without a covenant is 0^32, so a type-2 state owned by 0^32 is unlocked by
    // any plain input on 2433; 2732 refuses a zero type-2 owner on inputs. Creating such an output needs the sender's
    // own authorisation (it is a self-inflicted "burn" that is not one); no KOB path pins a zero owner.
    for (tpl, k) in templates() {
        let a = keypair();
        let next = vec![ks([0u8; 32], T_COVID, 1000)];
        let t = Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 1000)), p2pk(&a)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "holder sends to a zero type-2 owner (both programs allow the output)", &t));
        let s = ks([0u8; 32], T_COVID, 1000);
        let next = vec![ks([0x66; 32], T_ADDR, 1000)];
        let t = Tx { ins: vec![tok(s), plain()], outs: tok_outs(&k, &next, 0), next, wit: vec![1], sig_keys: vec![] };
        let ok = run(tpl, &k, "type-2 owner 0^32 spent by a stranger", &t);
        assert_eq!(ok, tpl == "kron-2433", "{tpl}: zero-owner type 2 is spendable by anyone only on the 2433 program");
    }
}

#[test]
fn r_kr_06_owner_equal_to_the_token_covenant_is_anyone_can_spend() {
    // info: a type-2 owner equal to the token's own covenant id is unlocked by the token input itself. KOB never
    // delivers or escrows under that owner (custody owner = the order's covenant id).
    for (tpl, k) in templates() {
        let s = ks(TOKEN_COV.as_bytes(), T_COVID, 1000);
        let next = vec![ks([0x66; 32], T_ADDR, 1000)];
        let t = Tx { ins: vec![tok(s)], outs: tok_outs(&k, &next, 0), next, wit: vec![0], sig_keys: vec![] };
        assert!(run(tpl, &k, "type-2 owner == own token covenant: anyone can spend", &t));
    }
}

#[test]
fn r_kr_07_output_id_type_range() {
    // (b) 2433 accepts an output with id_type 7 (no branch can ever spend it: a burn by the sender); 2732 bounds
    // the output type to 0..=3. Only the sender's own tokens can be sent there.
    for (tpl, k) in templates() {
        let a = keypair();
        let next = vec![ks([0x41; 32], 7, 1000)];
        let t = Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 1000)), p2pk(&a)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        let ok = run(tpl, &k, "output id_type 7 (unspendable)", &t);
        assert_eq!(ok, tpl == "kron-2433", "{tpl}");
        // an id_type 7 UTXO can never be spent on either program
        let t = Tx {
            ins: vec![tok(ks(pk(&a), 7, 1000)), p2pk(&a)],
            outs: tok_outs(&k, &[ks([0x41; 32], T_ADDR, 1000)], 0),
            next: vec![ks([0x41; 32], T_ADDR, 1000)],
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "spend an id_type 7 UTXO", &t));
    }
}

// ---------------------------------------------------------------- slots, strays, columns, signatures

#[test]
fn r_kr_08_slot_limits_strays_and_template_substitution() {
    for (tpl, k) in templates() {
        let kps: Vec<Keypair> = (0..5).map(|_| keypair()).collect();
        let mk = |n: usize, n_out: usize| -> Tx {
            let mut ins: Vec<In> = (0..n).map(|i| tok(ks(pk(&kps[i]), T_ADDR, 1000))).collect();
            ins.extend((0..n).map(|i| p2pk(&kps[i])));
            let total = 1000 * n as i64;
            let mut next: Vec<KS> = (0..n_out).map(|j| ks([0x41 + j as u8; 32], T_ADDR, total / n_out as i64)).collect();
            next[0].amount += total - (total / n_out as i64) * n_out as i64;
            Tx { outs: tok_outs(&k, &next, 0), next, wit: (0..n).map(|i| (n + i) as u8).collect(), ins, sig_keys: vec![] }
        };
        assert!(run(tpl, &k, "4 token inputs / 5 outputs (registry max)", &mk(4, 5)));
        assert!(!run(tpl, &k, "5 token inputs", &mk(5, 1)));
        assert!(!run(tpl, &k, "6 token outputs", &mk(1, 6)));
        // a stray covenant output (OpTrue) next to the described ones
        let mut t = mk(1, 1);
        t.outs.push(TransactionOutput {
            value: KAS as u64,
            script_public_key: pay_to_script_hash_script(&[OpTrue]),
            covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: TOKEN_COV }),
        });
        assert!(!run(tpl, &k, "stray covenant output", &t));
        // template substitution: the output under the OTHER KRON template
        let other = if tpl == "kron-2433" { Kron::load(TPL_2732) } else { Kron::load(TPL_2433) };
        let mut t = mk(1, 1);
        t.outs = tok_outs(&other, &t.next, 0);
        assert!(!run(tpl, &k, "output under the other KRON template", &t));
    }
}

#[test]
fn r_kr_09_every_token_input_checks_the_whole_group() {
    for (tpl, k) in templates() {
        let (a, b) = (keypair(), keypair());
        let next = vec![ks([0x41; 32], T_ADDR, 2000)];
        let lie = vec![ks([0x41; 32], T_ADDR, 1000), ks([0x42; 32], T_ADDR, 1000)];
        let mut ins = vec![tok(ks(pk(&a), T_ADDR, 1000)), tok(ks(pk(&b), T_ADDR, 1000)), p2pk(&a), p2pk(&b)];
        ins[1].role = Role::Tok { s: ks(pk(&b), T_ADDR, 1000), next: Some(lie), wit: None };
        let t = Tx { ins, outs: tok_outs(&k, &next, 0), next: next.clone(), wit: vec![2, 3], sig_keys: vec![] };
        assert!(!run(tpl, &k, "second token input carries different next columns", &t));
        let mut ins = vec![tok(ks(pk(&a), T_ADDR, 1000)), tok(ks(pk(&b), T_ADDR, 1000)), p2pk(&a), p2pk(&b)];
        ins[1].role = Role::Tok { s: ks(pk(&b), T_ADDR, 1000), next: None, wit: Some(vec![3, 3]) };
        let t = Tx { ins, outs: tok_outs(&k, &next, 0), next: next.clone(), wit: vec![2, 3], sig_keys: vec![] };
        assert!(!run(tpl, &k, "second input's witness column mis-authorises input 0", &t));
        // b's tokens authorised by a's key
        let t = Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 1000)), tok(ks(pk(&b), T_ADDR, 1000)), p2pk(&a)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 2],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "b's type-3 input authorised by a's P2PK input", &t));
    }
}

#[test]
fn r_kr_10_type0_signatures() {
    for (tpl, k) in templates() {
        let (a, b) = (keypair(), keypair());
        let next = vec![ks([0x41; 32], T_ADDR, 2000)];
        let ins = vec![tok(ks(pk(&a), T_PUBKEY, 1000)), tok(ks(pk(&b), T_PUBKEY, 1000))];
        let t = Tx {
            ins: ins.clone(),
            outs: tok_outs(&k, &next, 0),
            next: next.clone(),
            wit: vec![0, 0],
            sig_keys: vec![Some(a), Some(b)],
        };
        assert!(run(tpl, &k, "two type-0 inputs, both owners sign", &t));
        let t = Tx {
            ins: ins.clone(),
            outs: tok_outs(&k, &next, 0),
            next: next.clone(),
            wit: vec![0, 0],
            sig_keys: vec![Some(a), Some(a)],
        };
        assert!(!run(tpl, &k, "type-0 slot of b signed by a", &t));
        let t = Tx { ins, outs: tok_outs(&k, &next, 0), next, wit: vec![0, 0], sig_keys: vec![Some(a), None] };
        assert!(!run(tpl, &k, "type-0 slot of b missing", &t));
    }
}

// ---------------------------------------------------------------- id_type 3 (address presence): KOB maker deliveries

#[test]
fn r_kr_11_type3_needs_an_owner_p2pk_input_and_relies_on_sighash_all() {
    for (tpl, k) in templates() {
        let (owner, thief) = (keypair(), keypair());
        let next = vec![ks(pk(&thief), T_ADDR, 1000)];
        // no P2PK input of the owner: refused (a matcher or thief cannot move a maker's delivered tokens)
        let t = Tx {
            ins: vec![tok(ks(pk(&owner), T_ADDR, 1000)), p2pk(&thief)],
            outs: tok_outs(&k, &next, 0),
            next: next.clone(),
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "type-3 balance, only the thief's P2PK input", &t));
        let t = Tx {
            ins: vec![tok(ks(pk(&owner), T_ADDR, 1000)), cov_in(Hash::from_bytes(pk(&owner)))],
            outs: tok_outs(&k, &next, 0),
            next: next.clone(),
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(!run(tpl, &k, "type-3 balance, a covenant input whose id equals the owner key", &t));
        // residual (documented in the registry risks): a P2PK input the owner signed with SIGHASH_NONE, or with
        // SINGLE|ANYONECANPAY (0x84), for some other purpose authorises every type-3 balance of that key.
        let t = Tx {
            ins: vec![p2pk_ht(&owner, 0x02), tok(ks(pk(&owner), T_ADDR, 1000))],
            outs: tok_outs(&k, &next, 1),
            next: next.clone(),
            wit: vec![0],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "type-3 balance taken with an owner SIGHASH_NONE input", &t));
        let mut outs = vec![TransactionOutput { value: (9 * KAS) as u64, script_public_key: p2pk_spk(&pk(&owner)), covenant: None }];
        outs.extend(tok_outs(&k, &next, 1));
        let t = Tx { ins: vec![p2pk_ht(&owner, 0x84), tok(ks(pk(&owner), T_ADDR, 1000))], outs, next, wit: vec![0], sig_keys: vec![] };
        assert!(run(tpl, &k, "type-3 balance taken with an owner SINGLE|ANYONECANPAY input (output 0 = the owner's own)", &t));
    }
}

#[test]
fn r_kr_12_kob_signers_use_sighash_all_only() {
    // every KOB signing path produces SIGHASH_ALL (`sign_digest`), and every wallet signature goes through
    // `normalize_signature` in `assemble`/`finalize`, which refuses any other hash type (web: `collectSignatures`,
    // x402: `exact.rs` canonical SIGHASH_ALL sigscripts).
    use kob_protocol::tx::{normalize_signature, sign_digest};
    let sig = sign_digest(&[7u8; 32], &[9u8; 32]).expect("sign");
    assert_eq!((sig.len(), sig[64]), (65, 0x01), "local signer: 64-byte Schnorr + SIGHASH_ALL");
    assert!(normalize_signature(0, &sig).is_ok());
    assert_eq!(normalize_signature(0, &sig[..64]).expect("64-byte signature")[64], 0x01, "a bare signature is taken as SIGHASH_ALL");
    for ht in [0x02u8, 0x04, 0x81, 0x82, 0x84] {
        let mut bad = sig.clone();
        bad[64] = ht;
        assert!(normalize_signature(0, &bad).is_err(), "65-byte signature with hash type {ht:#04x}");
        let pushed = [vec![0x41u8], bad].concat();
        assert!(normalize_signature(0, &pushed).is_err(), "pushed signature with hash type {ht:#04x}");
    }
}

// ---------------------------------------------------------------- genesis check on KRON shapes

fn genesis(k: &Kron, outs: &[(u32, Vec<u8>)]) -> (String, Vec<GenesisOutput>) {
    let _ = k;
    let outpoint = TransactionOutpoint { transaction_id: TransactionId::from_bytes([0x99; 32]), index: 0 };
    let txouts: Vec<(u32, TransactionOutput)> = outs
        .iter()
        .map(|(i, redeem)| {
            (*i, TransactionOutput { value: KAS as u64, script_public_key: pay_to_script_hash_script(redeem), covenant: None })
        })
        .collect();
    let cid = covenant_id(outpoint, txouts.iter().map(|(i, o)| (*i, o)));
    let g = txouts
        .iter()
        .zip(outs)
        .map(|((i, o), (_, redeem))| GenesisOutput {
            index: *i,
            value: o.value,
            script_public_key: o.script_public_key.clone(),
            redeem_script: Some(redeem.clone()),
        })
        .collect();
    (hexs(&cid.as_bytes()), g)
}

#[test]
fn r_kr_13_genesis_check_covers_the_kron_shapes() {
    let reg = Registry::parse(DEFAULT_REGISTRY_JSON).expect("registry");
    let txid = "11".repeat(32);
    let auth = ([0x99u8; 32], 0u32);
    for (id, k) in templates() {
        let tpl = reg.template(id).expect("template");
        let curve = ks([0xc1; 32], T_COVID, 1_000_000_000);
        // clean: one curve-owned (type 2) output
        let (cid, outs) = genesis(&k, &[(0, k.redeem(&curve))]);
        let r = verify_genesis(tpl, &cid, &txid, auth, &outs).expect("clean KRON genesis");
        assert_eq!((r.outputs, r.supply), (1, 1_000_000_000));
        // clean but with a minter output: accepted; the report carries no minter information (gap G-1 of the review:
        // a genesis minter is a live mint authority and the registry cannot see it from the report)
        let (cid, outs) = genesis(&k, &[(0, k.redeem(&curve)), (1, k.redeem(&minter([0xc1; 32], T_COVID, 0)))]);
        let r = verify_genesis(tpl, &cid, &txid, auth, &outs).expect("genesis with a minter");
        println!(
            "B2 {id} genesis with an is_minter=1 output: ACCEPTED, report outputs {} supply {} (no minter field)",
            r.outputs, r.supply
        );
        // hidden non-template output
        let (cid, outs) = genesis(&k, &[(0, k.redeem(&curve)), (1, vec![OpTrue])]);
        assert!(
            matches!(verify_genesis(tpl, &cid, &txid, auth, &outs), Err(GenesisError::NotTemplate(1, _))),
            "{id}: hidden OpTrue output"
        );
        // non-canonical state span (push opcode of the id_type field changed): the program would parse other values
        let mut odd = k.redeem(&curve);
        odd[33] = 0x02;
        let (cid, outs) = genesis(&k, &[(0, odd)]);
        assert!(
            matches!(verify_genesis(tpl, &cid, &txid, auth, &outs), Err(GenesisError::BadState(0, _))),
            "{id}: non-canonical state span"
        );
        // incomplete group: the covenant id covers two outputs, only one is shown
        let (cid, outs) = genesis(&k, &[(0, k.redeem(&curve)), (1, k.redeem(&curve))]);
        assert!(
            matches!(verify_genesis(tpl, &cid, &txid, auth, &outs[..1]), Err(GenesisError::NotTheGroup(1))),
            "{id}: partial group"
        );
        // unrevealed redeem script
        let (cid, mut outs) = genesis(&k, &[(0, k.redeem(&curve))]);
        outs[0].redeem_script = None;
        assert!(matches!(verify_genesis(tpl, &cid, &txid, auth, &outs), Err(GenesisError::Unrevealed(0))), "{id}: unrevealed");
        // an output under the OTHER KRON program is not an instance of this one
        let other = if id == "kron-2433" { Kron::load(TPL_2732) } else { Kron::load(TPL_2433) };
        let (cid, outs) = genesis(&k, &[(0, other.redeem(&curve))]);
        assert!(matches!(verify_genesis(tpl, &cid, &txid, auth, &outs), Err(GenesisError::NotTemplate(0, _))), "{id}: other program");
    }
}

// ---------------------------------------------------------------- order genesis group (genesis-sibling, KOB side)

/// genesis-sibling (closed): `payload::recover_orders` must refuse a placement whose genesis group holds anything
/// besides the order output. Takes a golden placement, checks the honest one is recovered with a
/// one-output group, then (a) adds a sibling OpTrue output to the order's group (re-deriving the
/// consensus genesis id and rebinding the order, the sibling and the custody to it: a transaction
/// consensus would accept) and (b) binds an extra output to the order's id under another authorising
/// input. Both must be refused.
fn sibling_genesis_is_refused(name: &str) {
    use kob_protocol::artifacts::token_template_by_hash;
    use kob_protocol::payload::recover_orders;
    use kob_protocol::state::TokenState;
    use kob_protocol::tx::{spk_from_string, spk_to_string, CovenantJson, TxJson, TxOutputJson};

    let raw = std::fs::read_to_string(common::repo_root().join("crates/kob-protocol/vectors/golden.json")).expect("golden vectors");
    let golden: serde_json::Value = serde_json::from_str(&raw).expect("golden json");
    let v = golden["transactions"]
        .as_array()
        .expect("transactions")
        .iter()
        .find(|x| x["name"] == name)
        .unwrap_or_else(|| panic!("{name}"));
    let honest: TxJson = serde_json::from_value(v["built"]["tx"].clone()).expect("tx json");
    let rec = recover_orders(&honest).unwrap_or_else(|e| panic!("{name}: the honest placement is recovered: {e}"));
    assert_eq!(rec.len(), 1, "{name}");
    let (order_out, old_id) = (rec[0].output as usize, rec[0].covenant_id);
    let bound = honest.outputs.iter().filter(|o| o.covenant.as_ref().is_some_and(|c| c.covenant_id == old_id)).count();
    assert_eq!(bound, 1, "{name}: the builder's genesis group is the order output alone");
    let binding = honest.outputs[order_out].covenant.clone().expect("order output binding");
    let sibling = |covenant| TxOutputJson {
        value: 100_000_000,
        script_public_key: spk_to_string(&pay_to_script_hash_script(&[OpTrue])),
        covenant: Some(covenant),
    };

    // (a) a sibling in the order's genesis group, with the consensus genesis id of the two-output group
    let mut tx = honest.clone();
    tx.outputs.push(sibling(binding.clone()));
    let outpoint = {
        let inp = &tx.inputs[binding.authorizing_input as usize];
        TransactionOutpoint { transaction_id: TransactionId::from_bytes(inp.transaction_id), index: inp.index }
    };
    let group: Vec<(u32, TransactionOutput)> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.covenant.as_ref() == Some(&binding))
        .map(|(j, o)| {
            (
                j as u32,
                TransactionOutput {
                    value: o.value,
                    script_public_key: spk_from_string(&o.script_public_key).expect("spk"),
                    covenant: None,
                },
            )
        })
        .collect();
    assert_eq!(group.len(), 2, "{name}: order output + sibling");
    let new_id = covenant_id(outpoint, group.iter().map(|(j, o)| (*j, o))).as_bytes();
    for o in tx.outputs.iter_mut() {
        if let Some(c) = o.covenant.as_mut() {
            if c.covenant_id == old_id {
                c.covenant_id = new_id;
            }
        }
    }
    if let Some(c) = &rec[0].custody {
        let tt = token_template_by_hash(&rec[0].order.token_tpl_hash().expect("token-holding")).expect("token program");
        let st = TokenState::custody(rec[0].order.family(), c.state.amount(), new_id, c.state.extension());
        tx.outputs[c.output as usize].script_public_key = spk_to_string(&st.spk_with(tt));
    }
    let r = recover_orders(&tx);
    match &r {
        Ok(v) => println!(
            "genesis-sibling FINDING ({name}): a sibling in the genesis group is recovered as a valid order ({})",
            hexs(&v[0].covenant_id)
        ),
        Err(e) => println!("genesis-sibling ({name}): sibling in the genesis group refused: {e}"),
    }
    assert!(r.is_err(), "{name}: genesis-sibling: an order whose genesis group has a sibling output must not be recovered");

    // (b) an extra output bound to the order's id under another authorising input
    if let Some(other) = (0..honest.inputs.len() as u16).find(|&i| i != binding.authorizing_input) {
        let mut tx = honest.clone();
        tx.outputs.push(sibling(CovenantJson { authorizing_input: other, covenant_id: old_id }));
        let r = recover_orders(&tx);
        assert!(r.is_err(), "{name}: genesis-sibling: an order whose covenant id is bound to another output must not be recovered");
    }
}

#[test]
fn r_kr_14_order_genesis_group_with_a_sibling_output_is_refused() {
    // genesis-sibling (closed): a sibling OpTrue output in the order's genesis group would carry the order's covenant id forever and
    // unlock the order's id_type 2 escrow outside the order's rules (r_kr_04 dependency). KRON placements of every
    // token-holding and KAS-holding kind, and the pair orders (every pair kind) with a KRON base token.
    for name in [
        "kron.create.ask",
        "kron.create.bid",
        "kron.create.condAsk",
        "kron.create.condBid",
        "kron.create.ifdBid",
        "kron.create.ifdAsk.repeat",
        "pair.kron-kcc20.create.ask",
        "pair.kron-kcc20.create.bid",
        "pair.kron-kcc20.create.condAsk",
        "pair.kron-kcc20.create.condBid",
        "pair.kron-kcc20.create.ifdBid",
        "pair.kron-kcc20.create.ifdAsk",
        "pair.kron-kron.create.ask",
        "pair.kron-kron.create.bid",
        "pair.kron-kron.create.condAsk",
        "pair.kron-kron.create.condBid",
        "pair.kron-kron.create.ifdBid",
        "pair.kron-kron.create.ifdAsk",
    ] {
        sibling_genesis_is_refused(name);
    }
}

#[test]
fn r_kr_14k_kcc20_order_genesis_group_with_a_sibling_output_is_refused() {
    // genesis-sibling (closed), KCC-20 side: the scheme 0x04 escrow has the same dependency (r_kc_02).
    for name in [
        "create.ask",
        "create.bid",
        "create.condAsk",
        "create.condBid",
        "create.ifdBid",
        "create.ifdAsk.repeat",
        "create.ask.8x8.x402",
        "pair.create.ask",
        "pair.create.bid",
        "pair.create.condAsk",
        "pair.create.condBid",
        "pair.create.ifdBid",
        "pair.create.ifdAsk",
        "pair.kcc20-kron.create.ask",
        "pair.kcc20-kron.create.bid",
        "pair.kcc20-kron.create.condAsk",
        "pair.kcc20-kron.create.condBid",
        "pair.kcc20-kron.create.ifdBid",
        "pair.kcc20-kron.create.ifdAsk",
    ] {
        sibling_genesis_is_refused(name);
    }
}

// ---------------------------------------------------------------- minter lineage (registry condition C2)

#[test]
fn r_kr_15_a_minter_output_needs_a_minter_input() {
    // C2 (no live minter) for the listed KRON tokens rests on this: an is_minter output of a covenant id can only be created by
    // its genesis or by a transaction that spends a minter of it, because EVERY non-minter token input refuses minter outputs
    // (and enforces conservation over all token inputs). So a genesis without a minter output proves the token never has a
    // live minter (`kob_protocol::genesis_evidence`, `kob registry verify-genesis`). r_kr_01 shows the one-input case.
    for (tpl, k) in templates() {
        let (a, b, m) = (keypair(), keypair(), keypair());
        let two = |next: Vec<KS>| Tx {
            ins: vec![tok(ks(pk(&a), T_ADDR, 600)), tok(ks(pk(&b), T_ADDR, 400)), p2pk(&a), p2pk(&b)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 3],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "baseline: two holders merge 600+400", &two(vec![ks([0x41; 32], T_ADDR, 1000)])));
        assert!(!run(
            tpl,
            &k,
            "two non-minters create an is_minter=1 output (conserving)",
            &two(vec![ks([0x41; 32], T_ADDR, 999), minter([0x42; 32], T_ADDR, 1)])
        ));
        assert!(!run(
            tpl,
            &k,
            "two non-minters create a zero-amount minter",
            &two(vec![ks([0x41; 32], T_ADDR, 1000), minter([0x42; 32], T_ADDR, 0)])
        ));
        // a minter spent next to a non-minter cannot carry its flag through: the non-minter input refuses the minter output
        let mixed = |next: Vec<KS>| Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), tok(ks(pk(&a), T_ADDR, 1000)), p2pk(&m), p2pk(&a)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![2, 3],
            sig_keys: vec![],
        };
        assert!(!run(
            tpl,
            &k,
            "minter + non-minter keep the minter (conserving)",
            &mixed(vec![minter(pk(&m), T_ADDR, 0), ks(pk(&a), T_ADDR, 1000)])
        ));
        // positive control: minters alone may (capability mint-authority, r_kr_02)
        let next = vec![minter(pk(&m), T_ADDR, 0), minter([0x43; 32], T_ADDR, 5)];
        let t = Tx {
            ins: vec![tok(minter(pk(&m), T_ADDR, 0)), p2pk(&m)],
            outs: tok_outs(&k, &next, 0),
            next,
            wit: vec![1],
            sig_keys: vec![],
        };
        assert!(run(tpl, &k, "a minter alone clones itself (control)", &t));
    }
}
