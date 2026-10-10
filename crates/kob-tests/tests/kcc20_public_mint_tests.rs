//! KOB orders on tokens of the published build of the KCC-20 reference: the `KCC20` actor of upstream's `KCC20PublicMint`
//! app (argent-lang/kcc20-reference `c8a0871`, `fixtures/public-mint/sil/KCC20.sil`, vendored in
//! `contracts/third-party/kcc20-reference`), embedded as `TemplateId::Kcc20PublicMint`.
//!
//! Its state is `0x20 ‖ gen__kcc20_template` (the compiler-owned context: the program's own Sil template hash, the same in
//! every UTXO) followed by the standard 112-byte `KCC20State`. KOB pins the program as its KCC-1 actor-type handle, the
//! view upstream's own artifact exports for `actor_type<KCC20State>`: prefix = the 1-byte Sil prefix plus the context push
//! (34 B), the open state = the 112 bytes every KOB order reads at fixed offsets, suffix = the Sil suffix (3,885 B),
//! template hash = the handle's `734850b0...`. The order contracts are unchanged: they take (tokenTplHash, tplPrefixLen,
//! tplSuffixLen) per order and authenticate a holder as `blake3(len ‖ prefix ‖ len ‖ suffix)` of its redeem script.
//!
//! Executed in rusty-kaspa v2.1.0's TxScriptEngine (pinned templates, `pair_harness::Ed`):
//!   - the byte layout: the handle cut equals upstream's artifact, the 112 bytes after the context are the standalone
//!     build's state byte for byte, and every offset the orders read is the same;
//!   - every KAS-book scenario, branch shape and pair scenario on the published build (fills, refunds, IOC / FOK ends,
//!     updates, repeat merges, cancels; KobAsk / KobBid, conditional, if-done, pair kinds) passes on every input,
//!     including the token program's own leader / delegator template checks when several holders (custodies of
//!     different orders) are spent in one transaction (up to the program's 3 token inputs; a fourth is refused);
//!   - negatives: the same transactions with the custody / delivery tokens' context field replaced (a consistent holder of
//!     another `gen__kcc20_template` value under the same covenant id), or with the tokens moved to the standalone build
//!     (and the standalone scenarios moved to the published build), are refused by every order that settles against them;
//!     an order that pins the Sil cut (`9703112e...`, prefix 1) refuses the custody, and the builders refuse such terms.
//!
//! Run: cargo test --release -p kob-tests --test kcc20_public_mint_tests -- --nocapture

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use std::collections::{BTreeMap, BTreeSet};

use fx::*;
use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
use kaspa_consensus_core::hashing::sighash_type::SigHashType;
use kaspa_consensus_core::tx::{MutableTransaction, Transaction, UtxoEntry};
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::{build_with, Action};
use kob_protocol::script::{kcc20_state_value, p2sh_spk};
use kob_protocol::state::*;
use kob_protocol::tx::{sign_digest, SigPlan};
use ph::*;
use silverscript_abi::{encode_runtime_state_script, ArtifactValue};

const PM: TemplateId = TemplateId::Kcc20PublicMint;
const SA: TemplateId = TemplateId::Kcc20Ref;
const SIL_HASH: &str = "9703112ee6e3555107cd168858992b77d3b74f655205b2b463b1f9ec2ec73cf7";
const HANDLE_HASH: &str = "734850b0af0aeef49f009167bf9ddd238fd8c5cdbe214234f97181ac6f5cc498";
/// A context value that is not the program's template hash.
const OTHER_CTX: [u8; 32] = [0x5a; 32];

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn pinned() -> Subs {
    Subs { subs: BTreeMap::new() }
}

/// Every engine shape on program `p`: the KAS-book scenarios, the branch shapes, the pair scenarios with `p` on one or both
/// sides (against the 3/3 standalone reference and a KRON program), and the largest sweep the program's slots allow.
fn shapes(p: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    v.extend(scenarios_on(p).into_iter().map(|(n, a)| (format!("{}.{n}", p.name()), a)));
    v.extend(branches::branch_shapes(p).into_iter().map(|(n, a)| (format!("{}.{n}", p.name()), a)));
    for (pa, pb) in [(p, p), (p, TemplateId::Kcc20Ref), (TemplateId::KronToken2433, p)] {
        let tag = pair::pair_name(pa, pb);
        v.extend(pair::pair_scenarios(pa, pb).into_iter().map(|(n, a)| (format!("{tag}.{n}"), a)));
    }
    v.push((format!("{}.sweep3", p.name()), sweep(p, 2).expect("a batch")));
    v
}

/// `take.ask.partial` with `extra` more asks of one whole token: `1 + extra` custodies of different orders, one token leader
/// and `extra` delegators of the same token in one transaction.
fn sweep(p: TemplateId, extra: usize) -> Option<Action> {
    let (_, a) = scenarios_on(p).into_iter().find(|(n, _)| n == "take.ask.partial").expect("take.ask.partial");
    pad_batch(p, &a, extra, 0)
}

/// What an order input does in `e`: (kind, entry, fill amount).
fn what(e: &Ed, j: usize) -> (TemplateId, String, Option<i64>) {
    let SigPlan::Entry { template, entry, .. } = &e.plans[j] else { unreachable!() };
    (*template, entry.clone(), fill_n(&e.plans[j]))
}

fn label(w: &(TemplateId, String, Option<i64>)) -> String {
    let (t, entry, n) = w;
    let op = match (entry.as_str(), n) {
        ("settle" | "fill", Some(0)) => "refund".to_string(),
        ("settle" | "fill", Some(n)) if *n < 0 => "merge".to_string(),
        ("settle" | "fill", Some(_)) => "fill".to_string(),
        (other, _) => other.to_string(),
    };
    format!("{}.{op}", t.name())
}

/// Token covenant ids of `e` that run program `p`.
fn tokens_on(e: &Ed, p: TemplateId) -> Vec<[u8; 32]> {
    e.programs.iter().filter(|(_, t)| **t == p).map(|(c, _)| *c).collect()
}

/// The order inputs of `e` that must refuse a token group `tok` they do not recognise: the owners of its custodies (owner
/// scheme 0x04, not their maker's cancel), and the orders that take a delivery of it in a fill (bid-side kinds and pair
/// orders read the template of a token input to pin the delivery; an exit re-arming its repeating entry leaves the pin to
/// the entry, which merges the bought units into its custody).
fn readers(e: &Ed, tok: [u8; 32]) -> BTreeSet<usize> {
    let mut v = BTreeSet::new();
    for i in 0..e.plans.len() {
        if cov_of(&e.entries[i]) != Some(tok) || matches!(e.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }) {
            continue;
        }
        let TokenState::Kcc20(k) = e.in_tok(i) else { continue };
        if k.owner_scheme != SCHEME_COVID {
            continue;
        }
        if let Some(j) =
            (0..e.plans.len()).find(|&j| matches!(e.plans[j], SigPlan::Entry { .. }) && cov_of(&e.entries[j]) == Some(k.owner))
        {
            if what(e, j).1 != "cancel" {
                v.insert(j);
            }
        }
    }
    for j in 0..e.plans.len() {
        if !matches!(e.plans[j], SigPlan::Entry { .. }) {
            continue;
        }
        let (t, entry, n) = what(e, j);
        let buys = matches!(
            t,
            TemplateId::KobBid
                | TemplateId::KobCondBid
                | TemplateId::KobIfdBid
                | TemplateId::KobPair
                | TemplateId::KobCondPair
                | TemplateId::KobIfdPair
        );
        let (_, state) = e.entry_state(j);
        let fills = (entry == "settle" || entry == "fill") && n.is_some_and(|n| n > 0);
        let merged = (0..e.plans.len()).any(|k| matches!(e.plans[k], SigPlan::Entry { .. }) && label(&what(e, k)).ends_with(".merge"));
        if buys && fills && !merged && state.windows(32).any(|w| w == tok) {
            v.insert(j);
        }
    }
    v
}

fn sig(tx: &Transaction, entries: &[UtxoEntry], i: usize, key: &[u8; 32], ht: u8) -> Vec<u8> {
    let mt = MutableTransaction::with_entries(tx.clone(), entries.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let d = calc_schnorr_signature_hash(&mt.as_verifiable(), i, SigHashType::from_u8(ht).unwrap(), &reused).as_bytes();
    let mut s = sign_digest(key, &d).unwrap();
    s[64] = ht;
    s
}

/// `e` assembled, then every holder of token `tok` (inputs and outputs, all on the published build) rewritten to carry
/// `ctx` in its context field (a consistent holder of another `gen__kcc20_template` value: its own transfer accepts its
/// continuations), and every signature made again over the result.
fn with_context(e: &Ed, tok: [u8; 32], ctx: [u8; 32]) -> (Transaction, Vec<UtxoEntry>) {
    let tpl = token_template(PM);
    let swap = |r: &[u8]| {
        let mut v = r.to_vec();
        v[2..34].copy_from_slice(&ctx);
        v
    };
    let (tx0, e0) = e.finish(&pinned());
    let (mut tx, mut entries) = (tx0.clone(), e0.clone());
    let len = tpl.prefix.len() + tpl.state_len + tpl.suffix.len();
    for (i, (input, x)) in tx.inputs.iter_mut().zip(entries.iter_mut()).enumerate() {
        if cov_of(x) != Some(tok) || matches!(e.plans[i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }) {
            continue;
        }
        let ss = input.signature_script.clone();
        let at = ss.len() - len;
        assert!(tpl.state_of(&ss[at..]).is_some(), "{}: input {i} redeem", e.name);
        let new = swap(&ss[at..]);
        input.signature_script = [&ss[..at], new.as_slice()].concat();
        *x = UtxoEntry::new(x.amount, p2sh_spk(&new), x.block_daa_score, x.is_coinbase, x.covenant_id);
    }
    for (k, (c, st)) in &e.tok_out {
        if *c == tok {
            let r = tpl.redeem(&st.as_kcc20().expect("kcc20").encode());
            tx.outputs[*k].script_public_key = p2sh_spk(&swap(&r));
        }
    }
    let keys = keys();
    for i in 0..tx.inputs.len() {
        let Some(signer) = e.plans[i].signer() else { continue };
        let key = keys[&e.signers.get(&i).map(|k| pk(*k)).unwrap_or(signer)];
        let ht = e.sighash_types.get(&i).copied().unwrap_or(1);
        let (old, new) = (sig(&tx0, &e0, i, &key, ht), sig(&tx, &entries, i, &key, ht));
        let ss = &mut tx.inputs[i].signature_script;
        let at = ss.windows(65).position(|w| w == old.as_slice()).unwrap_or_else(|| panic!("{}: input {i} signature", e.name));
        ss[at..at + 65].copy_from_slice(&new);
    }
    (tx, entries)
}

fn exec(tx: &Transaction, entries: &[UtxoEntry]) -> Vec<Result<u64, String>> {
    kob_protocol::verify::execute(tx, entries, false).expect("tx-level")
}

fn failing(res: &[Result<u64, String>]) -> BTreeSet<usize> {
    res.iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect()
}

// ---------------------------------------------------------------- layout

#[test]
fn a_published_holder_is_the_handle_prefix_then_the_standard_112_byte_state() {
    let t = template(PM);
    let pm = token_template(PM);
    let sa = token_template(SA);
    let c = t.contract();
    let bc = &c.compiled.bytecode;
    // the Sil cut (what silverc records) and the handle cut (what KOB pins)
    assert_eq!((c.compiled.state_span.offset, c.compiled.state_span.len, bc.len()), (1, 145, 4_031));
    assert_eq!((hexs(&t.sil_hash), hexs(&c.compiled.template_hash)), (SIL_HASH.to_string(), SIL_HASH.to_string()));
    assert_eq!(hexs(&pm.hash), HANDLE_HASH);
    assert_eq!((pm.prefix.len(), pm.state_len, pm.suffix.len()), (34, 112, 3_885));
    assert_eq!(pm.prefix, [&bc[..1], &[0x20u8][..], &t.sil_hash[..]].concat(), "prefix = Sil prefix ‖ push32 ‖ gen__kcc20_template");
    assert_eq!(pm.suffix, bc[146..]);
    assert_eq!((pm.prefix[0], sa.prefix.as_slice()), (0x6b, &[0x6bu8][..]), "both builds open with the same byte");
    assert_eq!(pm.hash, silverscript_abi::template_hash(&pm.prefix, &pm.suffix));

    // the published state of a holder, encoded by the Sil ABI over the full runtime state (context included) ...
    let st = Kcc20State {
        amount: 0x0102_0304_0506_0708,
        owner: [0x11; 32],
        owner_scheme: SCHEME_COVID,
        borrow_scheme: 0,
        borrow_guard: [0x22; 32],
        extension_commitment: [0xee; 32],
    };
    let ArtifactValue::Object(mut m) = kcc20_state_value(&st) else { unreachable!() };
    m.insert("gen__kcc20_template".into(), t.sil_hash.to_vec().into());
    let full = encode_runtime_state_script(&t.artifact, &c.runtime_state, &m).unwrap();
    let s112 = st.encode();
    // ... is the context push followed by the standalone build's 112 bytes, byte for byte
    assert_eq!(full, [&[0x20u8][..], &t.sil_hash[..], &s112[..]].concat());
    assert_eq!(sa.state_of(&sa.redeem(&s112)), Some(s112.as_slice()));
    let redeem = pm.redeem(&s112);
    assert_eq!(redeem, [&bc[..1], &full[..], &bc[146..]].concat(), "the handle cut rebuilds the Sil instance");
    assert_eq!(pm.state_of(&redeem), Some(s112.as_slice()));
    // the offsets every order reads in the 112 bytes (KobAsk.settle): amount [1..9) owner [10..42) owner_scheme 43
    // borrow_scheme 45 borrow_guard [47..79) extension_commitment [80..112), push opcodes in between
    let s = &redeem[34..146];
    assert_eq!((s[0], s[9], s[42], s[44], s[46], s[79]), (0x08, 0x20, 0x01, 0x01, 0x20, 0x20));
    assert_eq!(i64::from_le_bytes(s[1..9].try_into().unwrap()), st.amount);
    assert_eq!((&s[10..42], s[43], s[45], &s[47..79], &s[80..112]), (&[0x11; 32][..], 4, 0, &[0x22; 32][..], &[0xee; 32][..]));
    assert_eq!(&s[43..46], &[0x04, 0x01, 0x00], "KobAsk's custody check `tin.slice(43, 46) == 0x040100`");
    // the stray guard locates the owner from the END of the redeem script: 10 - 112 - tplSuffixLen
    let end = redeem.len() as i64 + 10 - 112 - pm.suffix.len() as i64;
    assert_eq!(&redeem[end as usize..end as usize + 32], &[0x11; 32]);
    // a holder with another context value is not an instance of the pinned template
    let mut other = redeem.clone();
    other[2..34].copy_from_slice(&OTHER_CTX);
    assert!(pm.state_of(&other).is_none());
    println!(
        "LAYOUT prefix {} B = 6b 20 <{SIL_HASH}>, state 112 B at [34..146) = standalone state, suffix {} B; handle {HANDLE_HASH}",
        pm.prefix.len(),
        pm.suffix.len()
    );
}

// ---------------------------------------------------------------- the order grid

#[test]
fn every_order_shape_runs_on_the_published_build() {
    let subs = pinned();
    let run = Run { subs: &subs, pair: PM.name().into() };
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut on_pm = 0;
    for (name, a) in shapes(PM) {
        let e = Ed::new(&name, &built(a));
        if tokens_on(&e, PM).is_empty() {
            continue; // a creation without tokens, or a shape whose tokens run another program (the 8/8 batch fixtures)
        }
        on_pm += 1;
        run.ok(&e);
        for j in 0..e.plans.len() {
            if matches!(e.plans[j], SigPlan::Entry { .. }) {
                *seen.entry(label(&what(&e, j))).or_default() += 1;
            }
        }
    }
    println!("COVERAGE {on_pm} transactions with published-build tokens: {seen:?}");
    for want in [
        "KobAsk.fill",
        "KobAsk.refund",
        "KobAsk.cancel",
        "KobBid.fill",
        "KobCondAsk.fill",
        "KobCondBid.fill",
        "KobCondAsk.update",
        "KobIfdAsk.fill",
        "KobIfdBid.fill",
        "KobIfdAsk.merge",
        "KobIfdBid.merge",
        "KobPair.fill",
        "KobCondPair.fill",
        "KobIfdPair.fill",
    ] {
        assert!(seen.contains_key(want), "{want} not covered: {seen:?}");
    }
}

#[test]
fn custodies_of_three_orders_share_one_transfer_and_a_fourth_is_refused() {
    let subs = pinned();
    let run = Run { subs: &subs, pair: PM.name().into() };
    let e = Ed::new("sweep3", &built(sweep(PM, 2).unwrap()));
    let tok = TOKEN_COV;
    let holders: Vec<usize> = (0..e.plans.len())
        .filter(|i| cov_of(&e.entries[*i]) == Some(tok) && !matches!(e.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }))
        .collect();
    assert_eq!(holders.len(), 3, "a leader and two delegators");
    assert_eq!(readers(&e, tok).len(), 3, "three asks, each owning one of them");
    run.ok(&e);
    let four = sweep(PM, 3).unwrap();
    assert!(build_with(&four, &budgets).is_err(), "a fourth token input is beyond the program's 3 / 3 slots");
    // the token program's own checks: every holder refuses a leader that is not a KCC20 of its template, so the same
    // transaction with another context value fails at the two delegators too (and at the three orders)
    let (tx, entries) = with_context(&e, tok, OTHER_CTX);
    let bad = failing(&exec(&tx, &entries));
    let delegators: BTreeSet<usize> = holders[1..].iter().copied().collect();
    assert!(readers(&e, tok).is_subset(&bad) && delegators.is_subset(&bad), "failing {bad:?}");
    println!("SWEEP3 accepted; another context: inputs {bad:?} refuse (orders {:?}, delegators {delegators:?})", readers(&e, tok));
}

// ---------------------------------------------------------------- negatives

#[test]
fn another_context_field_is_refused_by_every_order_that_settles_against_it() {
    let (mut cases, mut checked) = (0, BTreeMap::<String, usize>::new());
    for (name, a) in shapes(PM) {
        let e = Ed::new(&name, &built(a));
        for tok in tokens_on(&e, PM) {
            let want = readers(&e, tok);
            if want.is_empty() {
                continue;
            }
            // control: the machinery with the program's own context value changes nothing
            let sil: [u8; 32] = template(PM).sil_hash;
            let (tx, entries) = with_context(&e, tok, sil);
            assert!(failing(&exec(&tx, &entries)).is_empty(), "{name}: control");
            let (tx, entries) = with_context(&e, tok, OTHER_CTX);
            let bad = failing(&exec(&tx, &entries));
            assert!(want.is_subset(&bad), "{name}: orders {want:?} must refuse; failing {bad:?}");
            cases += 1;
            for j in want {
                *checked.entry(label(&what(&e, j))).or_default() += 1;
            }
        }
    }
    println!("NEGATIVE context field: {cases} transactions, refused at {checked:?}");
    assert!(cases > 40, "{cases}");
}

/// Moves every token of program `from` in `e` to program `to` (inputs and outputs: the token transfer stays consistent).
fn move_program(e: &mut Ed, from: TemplateId, to: TemplateId) -> Vec<[u8; 32]> {
    let toks = tokens_on(e, from);
    for t in &toks {
        e.programs.insert(*t, to);
    }
    for (i, p) in e.plans.iter_mut().enumerate() {
        if !toks.iter().any(|t| cov_of(&e.entries[i]) == Some(*t)) {
            continue;
        }
        match p {
            SigPlan::TokenLeader { template, .. } | SigPlan::TokenDelegator { template, .. } => *template = to,
            _ => {}
        }
    }
    toks
}

#[test]
fn the_standalone_build_and_the_published_build_are_different_tokens_to_an_order() {
    let subs = pinned();
    let mut cases = 0;
    for (from, to) in [(PM, SA), (SA, PM)] {
        for (name, a) in shapes(from) {
            let mut e = Ed::new(&name, &built(a));
            let toks = tokens_on(&e, from);
            let want: BTreeSet<usize> = toks.iter().flat_map(|t| readers(&e, *t)).collect();
            if want.is_empty() {
                continue;
            }
            move_program(&mut e, from, to);
            let (tx, entries) = e.finish(&subs);
            let bad = failing(&exec(&tx, &entries));
            assert!(want.is_subset(&bad), "{name} on {}: orders {want:?} must refuse; failing {bad:?}", to.name());
            cases += 1;
        }
    }
    println!("NEGATIVE program swap: {cases} transactions refused by their orders");
    assert!(cases > 80, "{cases}");
}

#[test]
fn an_order_that_pins_the_sil_cut_refuses_the_custody() {
    let sil: [u8; 32] = template(PM).sil_hash;
    let handle = token_template(PM).hash;
    // the builders know the program only by its handle
    assert_eq!(kob_protocol::build::token_program(&handle, 34, 3_885).unwrap(), PM);
    for (pre, suf) in [(1, 3_885), (1, 3_918), (34, 3_885)] {
        assert!(kob_protocol::build::token_program(&sil, pre, suf).is_err(), "Sil cut {pre}/{suf}");
    }
    assert!(kob_protocol::build::token_program(&handle, 1, 3_918).is_err());
    // in the engine: a refund whose order state names the Sil cut instead (prefix 1, the context counted in the suffix so
    // that the lengths still add up to the program) is refused by the order
    let subs = pinned();
    let run = Run { subs: &subs, pair: PM.name().into() };
    let mut cases = 0;
    for (name, a) in shapes(PM) {
        let e = Ed::new(&name, &built(a));
        for j in 0..e.plans.len() {
            if !matches!(e.plans[j], SigPlan::Entry { .. }) || label(&what(&e, j)) != "KobAsk.refund" {
                continue;
            }
            let (_, st) = e.entry_state(j);
            let AnyState::KobAsk(a) = AnyState::decode(TemplateId::KobAsk, &st).unwrap() else { unreachable!() };
            if a.token_tpl_hash != handle {
                continue;
            }
            for (pre, suf) in [(1, 3_918), (1, 3_885)] {
                let mut x = e.clone().named(&format!("{name}.sil-cut-{pre}-{suf}"));
                let odd = AskState { token_tpl_hash: sil, tpl_prefix_len: pre, tpl_suffix_len: suf, ..a.clone() };
                x.set_entry_state(j, AnyState::KobAsk(odd).encode());
                run.bad(&x, j);
                cases += 1;
            }
        }
    }
    assert!(cases >= 2, "{cases}");
}
