//! Review B2 (internal listing review): regression tests for KaspaCom's KCC20 0.2.5 token program
//! (`contracts/third-party/kaspacom-kcc20/KCC20.placeholder.json`, template `911f0638...`, 25,552 B, 112-byte state at
//! offset 1), run in rusty-kaspa v2.1.0's TxScriptEngine with the KIP-20 covenant context. The tests pin the program and
//! its registry entry and assert the refusals the listing verdict relies on for holders and KOB escrow (owner scheme 4):
//! owner authorisation of every transfer / burn input, conservation, output pinning, slot limits. `mint_by_owner` and
//! `mint_public` were reviewed by reading; the review report itself is not part of this repository. Finding K-1
//! (`set_public_mint_active` + a `transfer_delegator` input leaves one covenant output unchecked: the creator of a
//! mint_policy 2 token mints past `max_supply` or plants a non-program covenant UTXO) is PoC'd in r_kc_05..r_kc_07.
//!
//! Run: cargo test -p kob-tests --test review_b2_kaspacom -- --nocapture --test-threads=1
#![allow(clippy::needless_range_loop)]

mod common;

use std::collections::BTreeMap;

use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
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
use rand::{thread_rng, RngCore};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use silverscript_abi::{ArtifactValue, SilAbiArtifact};

use kob_protocol::registry::{Registry, DEFAULT_REGISTRY_JSON};

use common::{compiled_template_parts_and_hash, encode_entry_sig_script, push_redeem_script};

const KAS: i64 = 100_000_000;
const TOKEN_COV: Hash = Hash::from_bytes([0x70; 32]);
const ORDER_COV: Hash = Hash::from_bytes([0xc0; 32]);
const OTHER_COV: Hash = Hash::from_bytes([0xc1; 32]);
const EXT: [u8; 32] = [0xee; 32];
const SIGOP_SCRIPT_UNITS: u64 = 100_000;
const KASPACOM_ARTIFACT: &str = "contracts/third-party/kaspacom-kcc20/KCC20.placeholder.json";
const PINNED_HASH: &str = "911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c";

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

// ---------------------------------------------------------------- program and state

struct Token {
    art: SilAbiArtifact,
    prefix: Vec<u8>,
    suffix: Vec<u8>,
    hash: Vec<u8>,
}
impl Token {
    fn load() -> Token {
        let json = std::fs::read_to_string(common::repo_root().join(KASPACOM_ARTIFACT)).expect("read KaspaCom artifact");
        let art: SilAbiArtifact = serde_json::from_str(&json).expect("parse KaspaCom artifact");
        let (prefix, suffix, hash) = compiled_template_parts_and_hash(&art);
        assert_eq!((prefix.len(), suffix.len()), (1, 25_439), "template split around the 112-byte state at offset 1");
        assert_eq!(hexs(&hash), PINNED_HASH, "pinned template hash");
        Token { art, prefix, suffix, hash }
    }
    fn redeem(&self, s: &St) -> Vec<u8> {
        [self.prefix.clone(), s.bytes(), self.suffix.clone()].concat()
    }
    fn spk(&self, s: &St) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

#[derive(Clone, Copy, Debug)]
struct St {
    amount: i64,
    owner: [u8; 32],
    scheme: u8,
    borrow: u8,
    guard: [u8; 32],
    ext: [u8; 32],
}
fn st(amount: i64, owner: [u8; 32], scheme: u8) -> St {
    St { amount, owner, scheme, borrow: 0, guard: [0; 32], ext: EXT }
}
impl St {
    fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x08];
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.scheme, 0x01, self.borrow, 0x20]);
        v.extend_from_slice(&self.guard);
        v.push(0x20);
        v.extend_from_slice(&self.ext);
        assert_eq!(v.len(), 112);
        v
    }
    fn value(&self) -> ArtifactValue {
        BTreeMap::from([
            ("amount".to_string(), ArtifactValue::Int(self.amount)),
            ("owner".to_string(), self.owner.to_vec().into()),
            ("owner_scheme".to_string(), ArtifactValue::Byte(self.scheme)),
            ("borrow_scheme".to_string(), ArtifactValue::Byte(self.borrow)),
            ("borrow_guard".to_string(), self.guard.to_vec().into()),
            ("extension_commitment".to_string(), self.ext.to_vec().into()),
        ])
        .into()
    }
}

/// `KCC20MintExtensionV1` opening of a minter lane (kind 1). The minter UTXO's state is `amount 0, owner creator,
/// schemes 0, guard 0, ext = blake3(Packed(this))`; holders carry `ext = holder_ext() = blake3(domain ++ Packed(holder
/// fields))`. Packing: byte / fixed bytes raw, int as num2bin(.,8), bool as one byte (program @24593, @24653-24654, @24456).
#[derive(Clone, Copy, Debug)]
struct Ext {
    creator: [u8; 32],
    max_supply: i64,
    policy: u8,
    remaining: i64,
    active: bool,
}
const HOLDER_DOMAIN: &[u8; 24] = b"KASPACOM/KCC20/HOLDER/V1";
const TICKER: [u8; 32] = [0x54; 32];
const NAME: [u8; 32] = [0x4e; 32];
const TREASURY: [u8; 32] = [0x71; 32];
const FEE_RECIPIENT: [u8; 32] = [0x72; 32];
impl Ext {
    fn new(creator: [u8; 32]) -> Ext {
        Ext { creator, max_supply: 1_000_000, policy: 2, remaining: 1_000_000, active: false }
    }
    fn holder_packed(&self) -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&self.creator);
        v.extend_from_slice(&TICKER);
        v.extend_from_slice(&NAME);
        v.extend_from_slice(&1i64.to_le_bytes()); // display_scale
        v.extend_from_slice(&self.max_supply.to_le_bytes());
        v.extend_from_slice(&1i64.to_le_bytes()); // mint_lane_count
        v.push(self.policy);
        v.extend_from_slice(&0i64.to_le_bytes()); // mint_price_sompi
        v.extend_from_slice(&TREASURY);
        v.extend_from_slice(&FEE_RECIPIENT);
        v.extend_from_slice(&0i64.to_le_bytes()); // protocol_fee_bps
        v
    }
    fn holder_ext(&self) -> [u8; 32] {
        *blake3::hash(&[HOLDER_DOMAIN.as_slice(), &self.holder_packed()].concat()).as_bytes()
    }
    fn commit(&self) -> [u8; 32] {
        let mut v = self.holder_packed();
        v.extend_from_slice(&self.remaining.to_le_bytes());
        v.push(u8::from(self.active));
        v.extend_from_slice(&self.holder_ext());
        *blake3::hash(&v).as_bytes()
    }
    fn value(&self) -> ArtifactValue {
        BTreeMap::from([
            ("kind".to_string(), ArtifactValue::Byte(1)),
            ("creator".to_string(), self.creator.to_vec().into()),
            ("ticker".to_string(), TICKER.to_vec().into()),
            ("name".to_string(), NAME.to_vec().into()),
            ("display_scale".to_string(), ArtifactValue::Int(1)),
            ("max_supply".to_string(), ArtifactValue::Int(self.max_supply)),
            ("mint_lane_count".to_string(), ArtifactValue::Int(1)),
            ("mint_policy".to_string(), ArtifactValue::Byte(self.policy)),
            ("mint_price_sompi".to_string(), ArtifactValue::Int(0)),
            ("treasury".to_string(), TREASURY.to_vec().into()),
            ("protocol_fee_recipient".to_string(), FEE_RECIPIENT.to_vec().into()),
            ("protocol_fee_bps".to_string(), ArtifactValue::Int(0)),
            ("remaining_supply".to_string(), ArtifactValue::Int(self.remaining)),
            ("public_mint_active".to_string(), ArtifactValue::Bool(self.active)),
            ("holder_extension_commitment".to_string(), self.holder_ext().to_vec().into()),
        ])
        .into()
    }
    /// The minter lane's state for this opening.
    fn minter(&self) -> St {
        St { amount: 0, owner: self.creator, scheme: 0, borrow: 0, guard: [0; 32], ext: self.commit() }
    }
    /// The same opening with the public-mint switch flipped (what the minter's continuation commits to).
    fn toggled(&self) -> Ext {
        Ext { active: !self.active, ..*self }
    }
    /// A holder state of this token (scheme 0).
    fn holder(&self, amount: i64, owner: [u8; 32]) -> St {
        St { ext: self.holder_ext(), ..st(amount, owner, 0) }
    }
}

// ---------------------------------------------------------------- tx model

#[derive(Clone)]
enum Wit {
    Sig(Keypair),
    Empty,
}

#[derive(Clone)]
enum Role {
    /// `transfer(next, 0x00 ++ auth)` (leader, owner path)
    Transfer { s: St, next: Vec<St>, w: Wit },
    /// `transfer_delegator(auth)`
    Delegate { s: St, w: Wit },
    /// `burn(next, burn_amount, auth)` (leader)
    Burn { s: St, next: Vec<St>, burn: i64, w: Wit },
    /// `set_public_mint_active(extension, authority_signature, active, minter_output_index)` on a minter UTXO
    SetActive { s: St, e: Ext, active: bool, idx: i64, w: Wit },
    /// OpTrue-P2SH input carrying a covenant id (stands in for any input of that covenant, e.g. a KOB order)
    Presence,
}
#[derive(Clone)]
struct In {
    cov: Option<Hash>,
    role: Role,
}

fn transfer(s: St, next: Vec<St>, w: Wit) -> In {
    In { cov: Some(TOKEN_COV), role: Role::Transfer { s, next, w } }
}
fn delegate(s: St, w: Wit) -> In {
    In { cov: Some(TOKEN_COV), role: Role::Delegate { s, w } }
}
fn burn(s: St, next: Vec<St>, amount: i64, w: Wit) -> In {
    In { cov: Some(TOKEN_COV), role: Role::Burn { s, next, burn: amount, w } }
}
/// The creator flips the public-mint switch of the minter lane opened by `e`; its continuation is expected at `idx`.
fn set_active(e: Ext, idx: i64, w: Wit) -> In {
    In { cov: Some(TOKEN_COV), role: Role::SetActive { s: e.minter(), e, active: !e.active, idx, w } }
}
fn presence(c: Hash) -> In {
    In { cov: Some(c), role: Role::Presence }
}
fn role_name(r: &Role) -> &'static str {
    match r {
        Role::Transfer { .. } => "kcc20.transfer",
        Role::Delegate { .. } => "kcc20.transfer_delegator",
        Role::Burn { .. } => "kcc20.burn",
        Role::SetActive { .. } => "kcc20.set_public_mint_active",
        Role::Presence => "cov-presence",
    }
}
/// Token output (1 KAS carrier: the program requires at least 0.5 KAS per token output).
fn tok_out(t: &Token, s: &St, auth: u16) -> TransactionOutput {
    TransactionOutput {
        value: KAS as u64,
        script_public_key: t.spk(s),
        covenant: Some(CovenantBinding { authorizing_input: auth, covenant_id: TOKEN_COV }),
    }
}
/// The minter lane's continuation: same program, state `e.minter()`, carrying the input's full 10 KAS (@25385-25392).
fn minter_out(t: &Token, e: &Ext, auth: u16) -> TransactionOutput {
    TransactionOutput { value: (10 * KAS) as u64, ..tok_out(t, &e.minter(), auth) }
}
/// A covenant output of the token's covenant id whose script is NOT the program (P2SH of OpTrue).
fn foreign_out(auth: u16) -> TransactionOutput {
    TransactionOutput {
        value: KAS as u64,
        script_public_key: pay_to_script_hash_script(&[OpTrue]),
        covenant: Some(CovenantBinding { authorizing_input: auth, covenant_id: TOKEN_COV }),
    }
}
fn tok_outs(t: &Token, next: &[St]) -> Vec<TransactionOutput> {
    next.iter().map(|s| tok_out(t, s, 0)).collect()
}

fn sign(tx: &Transaction, entries: &[UtxoEntry], idx: usize, kp: &Keypair) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let h = calc_schnorr_signature_hash(&mt.as_verifiable(), idx, SigHashType::from_u8(1).expect("sighash all"), &reused);
    let msg = secp256k1::Message::from_digest_slice(h.as_bytes().as_slice()).expect("msg");
    let mut s = kp.sign_schnorr(msg).as_ref().to_vec();
    s.push(1);
    s
}

fn tok_ss(t: &Token, s: &St, entry: &str, args: &[ArtifactValue]) -> Vec<u8> {
    let mut ss = encode_entry_sig_script(&t.art, entry, args).unwrap_or_else(|e| panic!("encode {entry}: {e}"));
    ss.extend_from_slice(&push_redeem_script(&t.redeem(s)));
    ss
}

fn build(t: &Token, ins: &[In], outs: &[TransactionOutput]) -> (Transaction, Vec<UtxoEntry>) {
    let entries: Vec<UtxoEntry> = ins
        .iter()
        .map(|i| {
            let spk = match &i.role {
                Role::Transfer { s, .. } | Role::Delegate { s, .. } | Role::Burn { s, .. } | Role::SetActive { s, .. } => t.spk(s),
                Role::Presence => pay_to_script_hash_script(&[OpTrue]),
            };
            UtxoEntry::new((10 * KAS) as u64, spk, 0, false, i.cov)
        })
        .collect();
    let inputs: Vec<TransactionInput> = (0..ins.len())
        .map(|k| {
            TransactionInput::new_with_compute_budget(
                TransactionOutpoint { transaction_id: TransactionId::from_bytes([k as u8 + 1; 32]), index: k as u32 },
                vec![],
                0,
                0,
            )
        })
        .collect();
    let mut tx = Transaction::new(1, inputs, outs.to_vec(), 0, Default::default(), 0, vec![]);
    let unsigned = tx.clone();
    for (k, i) in ins.iter().enumerate() {
        let auth = |w: &Wit| match w {
            Wit::Sig(kp) => sign(&unsigned, &entries, k, kp),
            Wit::Empty => vec![],
        };
        tx.inputs[k].signature_script = match &i.role {
            Role::Transfer { s, next, w } => {
                let states = ArtifactValue::Array(next.iter().map(St::value).collect());
                tok_ss(t, s, "transfer", &[states, [vec![0u8], auth(w)].concat().into()])
            }
            Role::Delegate { s, w } => tok_ss(t, s, "transfer_delegator", &[auth(w).into()]),
            Role::Burn { s, next, burn, w } => {
                let states = ArtifactValue::Array(next.iter().map(St::value).collect());
                tok_ss(t, s, "burn", &[states, ArtifactValue::Int(*burn), auth(w).into()])
            }
            Role::SetActive { s, e, active, idx, w } => tok_ss(
                t,
                s,
                "set_public_mint_active",
                &[e.value(), auth(w).into(), ArtifactValue::Bool(*active), ArtifactValue::Int(*idx)],
            ),
            Role::Presence => push_redeem_script(&[OpTrue]),
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
fn run(t: &Token, name: &str, ins: &[In], outs: &[TransactionOutput]) -> bool {
    let (tx, entries) = build(t, ins, outs);
    match execute(&tx, &entries) {
        Err(e) => {
            println!("B2 kaspacom {name}: REJECTED ({e})");
            false
        }
        Ok(res) => {
            let mut why = String::new();
            for (k, r) in res.iter().enumerate() {
                if let Err(e) = r {
                    why.push_str(&format!(" in[{k}] {}: {e:?};", role_name(&ins[k].role)));
                }
            }
            let ok = why.is_empty();
            println!("B2 kaspacom {name}: {}{}", if ok { "ACCEPTED" } else { "REJECTED" }, why);
            ok
        }
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn r_kc_00_pinned_program_matches_the_registry_entry() {
    let t = Token::load();
    let reg = Registry::parse(DEFAULT_REGISTRY_JSON).expect("registry");
    let tpl = reg.template("kcc20-kaspacom-0-2-5").expect("template");
    assert_eq!(hexs(&t.hash), tpl.template_hash);
    assert_eq!((t.prefix.len() as u32, t.suffix.len() as u32, tpl.state_len), (tpl.prefix_len, tpl.suffix_len, 112));
    assert_eq!((tpl.max_token_inputs, tpl.max_token_outputs), (8, 8));
    assert_eq!((tpl.escrow.owner_scheme, tpl.escrow.borrow_scheme), (Some(4), Some(0)));
    let mut caps: Vec<&str> = tpl.capabilities.iter().map(|c| c.as_str()).collect();
    caps.sort();
    assert_eq!(caps, vec!["burn", "mint-authority", "public-mint"], "declared capabilities");
    // the ABI has exactly these six entries: no freeze, seize or blacklist entry exists
    let c = t.art.contracts.values().next().expect("one contract");
    let mut names: Vec<&str> = c.entries.keys().map(|s| s.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["burn", "mint_by_owner", "mint_public", "set_public_mint_active", "transfer", "transfer_delegator"]);
}

#[test]
fn r_kc_01_transfer_owner_auth_conservation_and_output_pinning() {
    let t = Token::load();
    let (a, b) = (keypair(), keypair());
    let (sa, sb) = (st(1000, pk(&a), 0), st(1000, pk(&b), 0));
    let n = vec![st(2000, [0x41; 32], 0)];
    assert!(run(
        &t,
        "baseline leader + delegate",
        &[transfer(sa, n.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))],
        &tok_outs(&t, &n)
    ));
    let inflated = vec![st(2001, [0x41; 32], 0)];
    assert!(!run(
        &t,
        "inflation by 1",
        &[transfer(sa, inflated.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))],
        &tok_outs(&t, &inflated)
    ));
    assert!(!run(
        &t,
        "delegate signed by the wrong key",
        &[transfer(sa, n.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(a))],
        &tok_outs(&t, &n)
    ));
    assert!(!run(
        &t,
        "leader signed by the wrong key",
        &[transfer(sa, n.clone(), Wit::Sig(b)), delegate(sb, Wit::Sig(b))],
        &tok_outs(&t, &n)
    ));
    assert!(!run(&t, "no leader (delegate at cov[0])", &[delegate(sa, Wit::Sig(a)), delegate(sb, Wit::Sig(b))], &tok_outs(&t, &n)));
    // an extra covenant output authorised by the delegate / an undescribed one authorised by the leader
    let mut outs = tok_outs(&t, &n);
    outs.push(TransactionOutput {
        value: KAS as u64,
        script_public_key: pay_to_script_hash_script(&[OpTrue]),
        covenant: Some(CovenantBinding { authorizing_input: 1, covenant_id: TOKEN_COV }),
    });
    assert!(!run(
        &t,
        "stray covenant output authorised by a delegate",
        &[transfer(sa, n.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))],
        &outs
    ));
    let mut outs = tok_outs(&t, &n);
    outs.push(TransactionOutput {
        value: KAS as u64,
        script_public_key: pay_to_script_hash_script(&[OpTrue]),
        covenant: Some(CovenantBinding { authorizing_input: 0, covenant_id: TOKEN_COV }),
    });
    assert!(!run(
        &t,
        "undescribed covenant output authorised by the leader",
        &[transfer(sa, n.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))],
        &outs
    ));
    // the extension commitment (asset identity) cannot change
    let mut other_ext = n.clone();
    other_ext[0].ext = [0xab; 32];
    assert!(!run(
        &t,
        "extension commitment changed",
        &[transfer(sa, other_ext.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))],
        &tok_outs(&t, &other_ext)
    ));
    // a token output below the 0.5 KAS carrier
    let mut low = tok_outs(&t, &n);
    low[0].value = 10_000_000;
    assert!(!run(&t, "token output carrier below 0.5 KAS", &[transfer(sa, n.clone(), Wit::Sig(a)), delegate(sb, Wit::Sig(b))], &low));
    // a zero-amount (minter-shaped) input cannot take part in a transfer
    let zero = st(0, pk(&b), 0);
    assert!(!run(
        &t,
        "zero-amount delegate",
        &[transfer(sa, vec![st(1000, [0x41; 32], 0)], Wit::Sig(a)), delegate(zero, Wit::Sig(b))],
        &tok_outs(&t, &[st(1000, [0x41; 32], 0)])
    ));
}

#[test]
fn r_kc_02_scheme4_escrow_needs_an_input_of_the_order_covenant() {
    let t = Token::load();
    let esc = st(1000, ORDER_COV.as_bytes(), 4);
    let n = vec![st(1000, [0x66; 32], 0)];
    let outs = tok_outs(&t, &n);
    assert!(!run(&t, "escrow as leader, no order input", &[transfer(esc, n.clone(), Wit::Empty)], &outs));
    assert!(!run(
        &t,
        "escrow as leader, input of another covenant",
        &[transfer(esc, n.clone(), Wit::Empty), presence(OTHER_COV)],
        &outs
    ));
    let thief = keypair();
    let n2 = vec![st(2000, pk(&thief), 0)];
    assert!(!run(
        &t,
        "escrow as delegate of a thief's transfer, no order input",
        &[transfer(st(1000, pk(&thief), 0), n2.clone(), Wit::Sig(thief)), delegate(esc, Wit::Empty)],
        &tok_outs(&t, &n2)
    ));
    assert!(!run(&t, "escrow as burn leader, no order input", &[burn(esc, vec![], 1000, Wit::Empty)], &[]));
    // DEPENDENCY: any input carrying the order covenant id releases the escrow; the order covenant is the guard
    assert!(run(
        &t,
        "escrow + any input of the order covenant (dependency)",
        &[transfer(esc, n.clone(), Wit::Empty), presence(ORDER_COV)],
        &outs
    ));
}

#[test]
fn r_kc_03_slot_limits() {
    let t = Token::load();
    let kps: Vec<Keypair> = (0..9).map(|_| keypair()).collect();
    let mk = |k: usize| -> (Vec<In>, Vec<TransactionOutput>) {
        let n = vec![st(1000 * k as i64, [0x41; 32], 0)];
        let mut ins = vec![transfer(st(1000, pk(&kps[0]), 0), n.clone(), Wit::Sig(kps[0]))];
        for i in 1..k {
            ins.push(delegate(st(1000, pk(&kps[i]), 0), Wit::Sig(kps[i])));
        }
        (ins, tok_outs(&t, &n))
    };
    let (ins, outs) = mk(8);
    assert!(run(&t, "8 token inputs (registry max)", &ins, &outs));
    let (ins, outs) = mk(9);
    assert!(!run(&t, "9 token inputs", &ins, &outs));
    let a = keypair();
    let n: Vec<St> = (0..9).map(|j| st(if j == 0 { 992 } else { 1 }, [0x41; 32], 0)).collect();
    assert!(!run(&t, "9 token outputs", &[transfer(st(1000, pk(&a), 0), n.clone(), Wit::Sig(a))], &tok_outs(&t, &n)));
}

#[test]
fn r_kc_04_burn_is_owner_authorised_and_conserving() {
    let t = Token::load();
    let (a, b) = (keypair(), keypair());
    let sa = st(1000, pk(&a), 0);
    let rest = vec![st(600, pk(&a), 0)];
    assert!(run(&t, "holder burns 400 of its own 1000", &[burn(sa, rest.clone(), 400, Wit::Sig(a))], &tok_outs(&t, &rest)));
    assert!(!run(&t, "burn signed by a non-owner", &[burn(sa, rest.clone(), 400, Wit::Sig(b))], &tok_outs(&t, &rest)));
    let more = vec![st(700, pk(&a), 0)];
    assert!(!run(&t, "burn 400 but keep 700", &[burn(sa, more.clone(), 400, Wit::Sig(a))], &tok_outs(&t, &more)));
    assert!(!run(&t, "burn more than the inputs", &[burn(sa, vec![], 1001, Wit::Sig(a))], &[]));
    // another holder's balance cannot be pulled into a burn without its owner
    let sb = st(1000, pk(&b), 0);
    let rest2 = vec![st(1000, pk(&a), 0)];
    assert!(!run(
        &t,
        "burn leader + another holder's delegate signed by the burner",
        &[burn(sa, rest2.clone(), 1000, Wit::Sig(a)), delegate(sb, Wit::Sig(a))],
        &tok_outs(&t, &rest2)
    ));
}

// ---------------------------------------------------------------- K-1: set_public_mint_active

/// K-1 PoC (hazard, ACCEPTED). `set_public_mint_active` (@24026) checks the creator signature over the opening, then
/// only `0 < covInCount <= 10` and `covInCount == covOutCount` (@24700-24717), finds its own position k among the
/// covenant inputs (@24718-25100), requires `cout(k) == minter_output_index` (@25106-25115) and pins that one output
/// (same program, toggled opening, input amount; @25116-25392). Nothing bounds `authOutCount(THIS)` or describes the
/// other covenant outputs. `transfer_delegator` (@13258) checks its owner authorisation, `cin(0) != THIS`,
/// `covInCount <= 8` and `authOutCount(THIS) == 0` (@13347-13357), but not which entry runs at cin(0) nor any
/// conservation. So a minter running `set_public_mint_active` plus one delegated holder cell makes covInCount ==
/// covOutCount == 2 and leaves one covenant output, authorised by the minter, entirely unchecked: the creator writes a
/// genuine-looking holder cell of any amount (here 10^6 x `max_supply`, `remaining_supply` untouched), which then
/// transfers like real supply. Who: the creator (minter signature) of a `mint_policy 2` token with a live lane
/// (`remaining_supply > 0`), using one token unit of his own (`mint_by_owner` is allowed under policy 2) or of any
/// holder who co-signs. The delegated unit is destroyed (unaccounted input).
#[test]
fn r_kc_05_set_public_mint_active_plus_delegator_mints_past_max_supply() {
    let t = Token::load();
    let (c, buyer, b) = (keypair(), keypair(), keypair());
    let e = Ext::new(pk(&c));
    // baseline: the creator flips the switch, one input, one output
    assert!(run(&t, "K-1 baseline: minter flips public mint", &[set_active(e, 0, Wit::Sig(c))], &[minter_out(&t, &e.toggled(), 0)]));
    // the PoC: one unit of the creator's own balance as delegator frees one covenant output
    let unit = e.holder(1, pk(&c));
    let forged = e.holder(1_000_000 * e.max_supply, pk(&c));
    assert!(run(
        &t,
        "K-1 HAZARD: set_public_mint_active + creator's 1-unit delegator -> holder of 10^6 x max_supply",
        &[set_active(e, 0, Wit::Sig(c)), delegate(unit, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), tok_out(&t, &forged, 0)]
    ));
    // the forged cell is indistinguishable from real supply: it transfers under the ordinary leader
    let sold = vec![e.holder(forged.amount, pk(&buyer))];
    assert!(run(
        &t,
        "K-1 HAZARD: forged cell transfers like real supply",
        &[transfer(forged, sold.clone(), Wit::Sig(c))],
        &tok_outs(&t, &sold)
    ));
    // any holder who co-signs serves as the delegator just as well (the holder's own signature is what it checks)
    assert!(run(
        &t,
        "K-1 HAZARD: a co-signing third-party holder as delegator",
        &[set_active(e, 0, Wit::Sig(c)), delegate(e.holder(1, pk(&b)), Wit::Sig(b))],
        &[minter_out(&t, &e.toggled(), 0), tok_out(&t, &forged, 0)]
    ));
}

/// K-1 PoC (hazard, ACCEPTED): the unchecked output need not be the program at all. The creator plants a covenant UTXO
/// that carries the token's covenant id under a script of his choice (here P2SH(OpTrue); a P2PK keeps it creator-only).
/// Consensus treats it as a continuation of the token covenant, so it later authorises token-covenant outputs of any
/// shape with no token program running: a permanent off-program mint key (same class as genesis contamination, but
/// created after genesis, so the registry's genesis check cannot see it). The free output must be authorised by the
/// minter: the delegator refuses to authorise any output (@13357).
#[test]
fn r_kc_06_set_public_mint_active_plants_a_non_program_covenant_output() {
    let t = Token::load();
    let c = keypair();
    let e = Ext::new(pk(&c));
    let unit = e.holder(1, pk(&c));
    assert!(run(
        &t,
        "K-1 HAZARD: set_public_mint_active + delegator -> non-program output with the token covenant id",
        &[set_active(e, 0, Wit::Sig(c)), delegate(unit, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), foreign_out(0)]
    ));
    let forged = e.holder(i64::MAX / 2, [0x41; 32]);
    assert!(run(
        &t,
        "K-1 HAZARD: the planted UTXO later authorises a token cell of any amount (no program runs)",
        &[presence(TOKEN_COV)],
        &[tok_out(&t, &forged, 0)]
    ));
    assert!(!run(
        &t,
        "free output authorised by the delegator instead of the minter",
        &[set_active(e, 0, Wit::Sig(c)), delegate(unit, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), foreign_out(1)]
    ));
}

/// K-1 limits (all REJECTED but one benign control): what the shape cannot do. A delegator is a holder cell
/// authorised by its own owner (signature schemes 0-2, P2SH input scheme 3, an input of the owner covenant for scheme 4;
/// @13365-13608), so the creator cannot draft another holder's balance or a KOB escrow (scheme 4) without its order
/// covenant; the delegator cannot lead (cin(0) != THIS, @13347). Without a delegator there is no free slot
/// (@24712-24717). Only the creator signs (@24681-24688), only `mint_policy 2` (@24674-24680) with
/// `remaining_supply > 0`. The minter's own continuation is pinned to the same program, amount 0, the creator, and the
/// toggled opening (@25116-25392). Two minter lanes each claim their own output (cout(k) for distinct k), so two lanes
/// alone free nothing.
#[test]
fn r_kc_07_set_public_mint_active_limits() {
    let t = Token::load();
    let (c, b, x) = (keypair(), keypair(), keypair());
    let e = Ext::new(pk(&c));
    let unit = e.holder(1, pk(&c));
    let forged = e.holder(1_000_000 * e.max_supply, pk(&c));
    let poc_outs = vec![minter_out(&t, &e.toggled(), 0), tok_out(&t, &forged, 0)];
    assert!(!run(
        &t,
        "third-party holder as delegator, signed by the creator",
        &[set_active(e, 0, Wit::Sig(c)), delegate(e.holder(1000, pk(&b)), Wit::Sig(c))],
        &poc_outs
    ));
    let escrow = St { ext: e.holder_ext(), ..st(1000, ORDER_COV.as_bytes(), 4) };
    assert!(!run(
        &t,
        "KOB escrow (scheme 4) as delegator, no order-covenant input",
        &[set_active(e, 0, Wit::Sig(c)), delegate(escrow, Wit::Empty)],
        &poc_outs
    ));
    assert!(!run(
        &t,
        "KOB escrow (scheme 4) as delegator, input of another covenant",
        &[set_active(e, 0, Wit::Sig(c)), delegate(escrow, Wit::Empty), presence(OTHER_COV)],
        &poc_outs
    ));
    assert!(!run(
        &t,
        "delegator at cin(0), minter at cin(1)",
        &[delegate(unit, Wit::Sig(c)), set_active(e, 1, Wit::Sig(c))],
        &[tok_out(&t, &forged, 1), minter_out(&t, &e.toggled(), 1)]
    ));
    assert!(!run(&t, "no delegator: minter + an extra output", &[set_active(e, 0, Wit::Sig(c))], &poc_outs));
    let mut two_free = poc_outs.clone();
    two_free.push(foreign_out(0));
    assert!(!run(&t, "one delegator, two free outputs", &[set_active(e, 0, Wit::Sig(c)), delegate(unit, Wit::Sig(c))], &two_free));
    assert!(!run(&t, "minter signed by a non-creator", &[set_active(e, 0, Wit::Sig(x)), delegate(unit, Wit::Sig(c))], &poc_outs));
    let p1 = Ext { policy: 1, ..e };
    assert!(!run(
        &t,
        "mint_policy 1 lane",
        &[set_active(p1, 0, Wit::Sig(c)), delegate(p1.holder(1, pk(&c)), Wit::Sig(c))],
        &[minter_out(&t, &p1.toggled(), 0), tok_out(&t, &p1.holder(forged.amount, pk(&c)), 0)]
    ));
    let spent = Ext { remaining: 0, ..e };
    assert!(!run(
        &t,
        "exhausted lane (remaining_supply 0)",
        &[set_active(spent, 0, Wit::Sig(c)), delegate(unit, Wit::Sig(c))],
        &[minter_out(&t, &spent.toggled(), 0), tok_out(&t, &forged, 0)]
    ));
    // the minter's own continuation is pinned
    let mut cont = minter_out(&t, &e.toggled(), 0);
    cont.script_public_key = t.spk(&St { amount: 1, ..e.toggled().minter() });
    assert!(!run(&t, "minter continuation with amount 1", &[set_active(e, 0, Wit::Sig(c))], &[cont]));
    let mut cont = minter_out(&t, &e.toggled(), 0);
    cont.script_public_key = t.spk(&St { owner: pk(&x), ..e.toggled().minter() });
    assert!(!run(&t, "minter continuation to another owner", &[set_active(e, 0, Wit::Sig(c))], &[cont]));
    assert!(!run(&t, "minter continuation not toggled", &[set_active(e, 0, Wit::Sig(c))], &[minter_out(&t, &e, 0)]));
    assert!(!run(&t, "minter continuation is not the program", &[set_active(e, 0, Wit::Sig(c))], &[foreign_out(0)]));
    // two minter lanes: each pins its own output, nothing is freed
    let e2 = Ext { remaining: 500_000, ..e };
    assert!(run(
        &t,
        "two lanes flip together (benign control)",
        &[set_active(e, 0, Wit::Sig(c)), set_active(e2, 1, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), minter_out(&t, &e2.toggled(), 1)]
    ));
    assert!(!run(
        &t,
        "two lanes, both claim output 0, second output forged",
        &[set_active(e, 0, Wit::Sig(c)), set_active(e2, 0, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), tok_out(&t, &forged, 0)]
    ));
    assert!(!run(
        &t,
        "two lanes plus a free third output",
        &[set_active(e, 0, Wit::Sig(c)), set_active(e2, 1, Wit::Sig(c))],
        &[minter_out(&t, &e.toggled(), 0), minter_out(&t, &e2.toggled(), 1), tok_out(&t, &forged, 0)]
    ));
}
