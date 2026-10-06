//! `KobPair` (contracts/v2/KobPair.sil, one template for both sides and both token families) security suite, executed in
//! the script engine against the template compiled from source (`common/pair_harness.rs`; an ablation run executes the
//! mutated covenant everywhere it appears).
//!
//! A pair order of A/B (A = `TOKEN_COV` on program `pa`, B = `TOKEN_B` on `pb`, both of scale 1,000 = `WHOLE`): side ASK
//! sells A (its custody S = A) for B (T), side BID pays B out of its escrow custody (S = B) for A (T). The order enforces
//! only its own terms: the ASK receives at least the ceil of B, the BID pays EXACTLY the floor of B and receives exactly n
//! of A, the custody is exact, strays of both tokens are refused, the outputs are positional. Positives are honest
//! `kob-protocol` builds (routes through the KAS books, netting with opposite pair orders, inventory fills where the
//! matcher's own tokens are the counterparty, decay, TWAP / DCA, refunds, cancels) on the four family mixes; attacks are
//! honest builds edited input by input (`ph::Ed`) or raw patches of the finished transaction (planted inputs), each
//! REJECTED by the order's own input, on the two mixed pairs (KCC-20/KRON and KRON/KCC-20: every family is S and T of
//! some side). Checks that exist once per token family carry a family suffix: `sk` / `sr` (the custody S is KCC-20 /
//! KRON), `tk` / `tr` (the bought token T).
//!
//! Scenario ids: `PP..` positives, `NP..` attacks (the first whitespace token of the name; tools/ablation/catalogs/pair.mjs).
//! Run: cargo test -p kob-tests --test kob_pair_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use fx::pair::*;
use fx::*;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, UtxoEntry};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::{Action, Batch, CancelOrder, RefundOrder};
use kob_protocol::family::Family;
use kob_protocol::script::{p2pk_spk, p2sh_spk, push_data};
use kob_protocol::state::*;
use kob_protocol::tx::{Arg, SigPlan, TokenUtxo, Witness};
use ph::*;

/// A third token (same programs as A / B; not a token of the pair).
pub const TOKEN_C: [u8; 32] = [0x72; 32];
/// The pair order under test (every scenario places it at covenant `X_ID`, UTXO tag 70, custody tag 71).
pub const OID: [u8; 32] = X_ID;
/// The order's UTXO DAA (the fixtures').
pub const ODAA: i64 = 1_000;
/// 90 days of DAA (the covenant's idle bound).
pub const MAX_IDLE: i64 = 77_760_000;
/// A price with a remainder: n * PR / 1000 rounds for every n not a multiple of 1000.
pub const PR: i64 = RATE + 1;
/// A fill amount with a remainder (4 whole A and one base unit): ceil and floor differ by one at `PR`.
pub const NR: i64 = 4 * WHOLE + 1;

// ---------------------------------------------------------------- fixtures

pub fn mixed() -> [(TemplateId, TemplateId); 2] {
    let m = family_mixes();
    [m[1], m[2]]
}

pub fn ftag(p: TemplateId) -> &'static str {
    if p.family() == Family::Kcc20 {
        "k"
    } else {
        "r"
    }
}

/// The program of the S token (custody) and the T token of a pair order.
pub fn s_prog(ask: bool, pa: TemplateId, pb: TemplateId) -> TemplateId {
    if ask {
        pa
    } else {
        pb
    }
}
pub fn t_prog(ask: bool, pa: TemplateId, pb: TemplateId) -> TemplateId {
    s_prog(!ask, pa, pb)
}
pub fn s_tok(ask: bool) -> [u8; 32] {
    if ask {
        TOKEN_COV
    } else {
        TOKEN_B
    }
}
pub fn t_tok(ask: bool) -> [u8; 32] {
    s_tok(!ask)
}
/// The side whose S (`want_s`) or T token is of family `fam` on a mixed pair.
pub fn side_with(fam: Family, want_s: bool, pa: TemplateId, _pb: TemplateId) -> bool {
    let ask_s = pa.family() == fam;
    if want_s {
        ask_s
    } else {
        !ask_s
    }
}

pub fn ask0(pa: TemplateId, pb: TemplateId) -> PairState {
    pair(MAKER_A, true, pa, pb, RATE)
}
pub fn bid0(pa: TemplateId, pb: TemplateId) -> PairState {
    pair(MAKER_A, false, pa, pb, RATE)
}
pub fn side0(ask: bool, pa: TemplateId, pb: TemplateId) -> PairState {
    pair(MAKER_A, ask, pa, pb, RATE)
}
/// A pair order at `PR` (rounding) with its custody for 10 whole A (a bid: its escrow for four fills).
pub fn side_pr(ask: bool, pa: TemplateId, pb: TemplateId) -> PairState {
    let mut s = pair(MAKER_A, ask, pa, pb, PR);
    if !ask {
        s.custody = s.bid_escrow(s.amount_left, 4).unwrap();
    }
    s
}
/// `s` with `left` base units left (an ask's custody follows; a bid keeps an escrow for four fills).
pub fn left_units(s: PairState, left: i64) -> PairState {
    let mut x = PairState { amount_left: left, ..s };
    x.custody = if x.is_ask() { left } else { x.bid_escrow(left, 4).unwrap() };
    x
}

pub fn user(p: TemplateId, amount: i64, key: u8) -> TokenState {
    tstate(p, amount, pk(key), false)
}

/// Inventory fill: the order (input 0, covenant `OID`) filled for `n` base units with the matcher's own tokens as the
/// counterparty (the matcher gives T from a key-held UTXO of 20 whole T and receives the released S).
pub fn inv(s: PairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    let ask = s.is_ask();
    let need = if ask { s.t_out_min(n, s.price_max()).unwrap() } else { n };
    let have = (need + WHOLE).max(20 * WHOLE);
    let have = if t_prog(ask, pa, pb).family() == Family::Kron { have.min(KRON_MAX) } else { have };
    let mut b = route(vec![pair_leg_amount(s, pa, pb, OID, 70, n)]);
    b.taker_tokens = vec![tutxo(t_prog(ask, pa, pb), t_tok(ask), 60, have, pk(MATCHER), false)];
    b
}

pub fn ed(name: &str, a: Action) -> Ed {
    Ed::new(name, &built(a))
}
pub fn edb(name: &str, b: Batch) -> Ed {
    Ed::new(name, &built(Action::Batch(b)))
}
pub fn edu(name: &str, b: Batch) -> Ed {
    Ed::new(name, &unchecked(b))
}

pub fn refund_of(s: PairState, pa: TemplateId, pb: TemplateId, lock: u64, value: u64) -> Action {
    let ask = s.is_ask();
    let custody = tutxo(s_prog(ask, pa, pb), s_tok(ask), 71, s.custody, OID, true);
    Action::RefundOrder(RefundOrder {
        order: order(70, value, OID, ODAA as u64, AnyState::KobPair(s)),
        foreign: vec![],
        custody: Some(custody),
        prefund: None,
        lock_time: lock,
        funding: vec![key_utxo(250, KEEPER, 5 * KAS)],
        change: Some(pk(KEEPER)),
        fee: fee(),
    })
}

pub fn cancel_of(s: PairState, pa: TemplateId, pb: TemplateId, strays: Vec<TokenUtxo>) -> Action {
    let ask = s.is_ask();
    let custody = tutxo(s_prog(ask, pa, pb), s_tok(ask), 71, s.custody, OID, true);
    Action::CancelOrder(CancelOrder {
        order: order(70, PV, OID, ODAA as u64, AnyState::KobPair(s)),
        custody: Some(custody),
        prefund: None,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![key_utxo(3, MAKER_A, 10 * KAS)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    })
}

// ---------------------------------------------------------------- edit helpers

pub fn pst(e: &Ed, i: usize) -> PairState {
    match AnyState::decode(TemplateId::KobPair, &e.entry_state(i).1).expect("pair state") {
        AnyState::KobPair(s) => s,
        _ => unreachable!(),
    }
}
/// The order input of covenant `OID`.
pub fn oin(e: &Ed) -> usize {
    e.input_of_cov(OID)
}
/// The custody input (settle argument 1).
pub fn cin(e: &Ed, i: usize) -> usize {
    e.arg_int(i, 1) as usize
}
/// The continuation output of covenant `c` (the order output bound to it), if any.
pub fn cont_of(e: &Ed, c: [u8; 32]) -> Option<usize> {
    e.bound_to(c).into_iter().find(|k| e.out_order(*k).is_some())
}
/// Applies `f` to the order's state (input `i`) and to its continuation's (keeping the continuation's amountLeft and
/// custody).
pub fn restate(e: &mut Ed, i: usize, f: impl Fn(&mut PairState)) {
    let mut s = pst(e, i);
    f(&mut s);
    e.set_entry_state(i, s.encode());
    let c = cov_of(&e.entries[i]).expect("order covenant");
    if let Some(k) = cont_of(e, c) {
        let (_, st) = e.out_order(k).unwrap();
        let AnyState::KobPair(mut x) = AnyState::decode(TemplateId::KobPair, &st).unwrap() else { unreachable!() };
        let (al, cu) = (x.amount_left, x.custody);
        f(&mut x);
        x.amount_left = al;
        x.custody = cu;
        e.set_out_spk_state(k, TemplateId::KobPair, &x.encode());
    }
}
/// Sets the continuation's state (output k) through `f`.
pub fn set_cont(e: &mut Ed, k: usize, f: impl Fn(&mut PairState)) {
    let (_, st) = e.out_order(k).expect("continuation");
    let AnyState::KobPair(mut x) = AnyState::decode(TemplateId::KobPair, &st).unwrap() else { unreachable!() };
    f(&mut x);
    e.set_out_spk_state(k, TemplateId::KobPair, &x.encode());
}
/// The token state of token input i.
pub fn in_tok(e: &Ed, i: usize) -> TokenState {
    match &e.plans[i] {
        SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. } => TokenState::Kcc20(state.clone()),
        SigPlan::KronToken { state, .. } => TokenState::Kron(state.clone()),
        p => panic!("{}: input {i} is not a token input: {p:?}", e.name),
    }
}
pub fn set_in_tok(e: &mut Ed, i: usize, st: TokenState) {
    match (&mut e.plans[i], st) {
        (SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. }, TokenState::Kcc20(k)) => *state = k,
        (SigPlan::KronToken { state, .. }, TokenState::Kron(k)) => *state = k,
        (p, _) => panic!("input {i}: {p:?} with a state of the other family"),
    }
}
/// The token output of token `tok` owned by `owner` (unique).
pub fn tok_out(e: &Ed, tok: [u8; 32], owner: [u8; 32]) -> usize {
    let v: Vec<usize> = e.tok_outs_of(tok).into_iter().filter(|k| e.out_state(*k).owner() == owner).collect();
    assert_eq!(v.len(), 1, "{}: token outputs of {:02x} owned by {:02x}: {v:?}", e.name, tok[0], owner[0]);
    v[0]
}
pub fn add_amt(e: &mut Ed, k: usize, d: i64) {
    let st = e.out_state(k);
    e.set_out_state(k, st.with_amount(st.amount() + d));
}
/// The matcher's KAS change (its largest plain output).
pub fn change(e: &Ed) -> usize {
    (0..e.tx.outputs.len())
        .filter(|k| {
            e.tx.outputs[*k].covenant.is_none()
                && [MATCHER, KEEPER].iter().any(|m| e.tx.outputs[*k].script_public_key == p2pk_spk(&pk(*m)))
        })
        .max_by_key(|k| e.tx.outputs[*k].value)
        .expect("the matcher's change")
}
/// Moves `d` sompi from the matcher's change to output k.
pub fn fund(e: &mut Ed, k: usize, d: i64) {
    let c = change(e);
    let (vc, vk) = (e.value(c) as i64, e.value(k) as i64);
    e.set_value(c, (vc - d) as u64);
    e.set_value(k, (vk + d) as u64);
}
/// A token output of `tok` (program `p`) for the matcher, its carrier paid from the change.
pub fn matcher_tok(e: &mut Ed, tok: [u8; 32], p: TemplateId, amount: i64) -> usize {
    e.programs.entry(tok).or_insert(p);
    let k = e.add_token_output(tok, user(p, amount, MATCHER), 0);
    fund(e, k, CARRIER as i64);
    k
}
/// Adds a stray of `tok` (program `p`, `amount`) owned by covenant `owner`, routed to the matcher.
pub fn add_stray(e: &mut Ed, tok: [u8; 32], p: TemplateId, owner: [u8; 32], amount: i64, tag: u8) -> usize {
    e.programs.entry(tok).or_insert(p);
    let i = e.add_token_input(utxo(tag, CARRIER, 1_000, Some(tok)), tstate(p, amount, owner, true), Witness::CovenantId);
    e.add_token_output(tok, user(p, amount, MATCHER), CARRIER);
    i
}
/// An output bound to covenant `c` (authorised by input `auth`) paying `v` sompi to the matcher's key: a forged UTXO of
/// that covenant id (the owner of its strays, a sibling).
pub fn forge_bound(e: &mut Ed, c: [u8; 32], auth: usize, v: u64) -> usize {
    let k = e.add_plain_output(0, MATCHER);
    fund(e, k, v as i64);
    e.tx.outputs[k].covenant =
        Some(kaspa_consensus_core::tx::CovenantBinding { authorizing_input: auth as u16, covenant_id: Hash::from_bytes(c) });
    k
}

// ---------------------------------------------------------------- raw patches (planted inputs)

/// Script public key `OP_DROP OP_TRUE` (not P2SH: its sigscript is one push, dropped).
pub fn drop_true() -> ScriptPublicKey {
    use kaspa_txscript::opcodes::codes::{OpDrop, OpTrue};
    ScriptPublicKey::new(0, vec![OpDrop, OpTrue].into())
}

/// Push-only filler of exactly `len` bytes (pushes of at most 300 bytes, minimal encodings).
pub fn filler(mut len: usize) -> Vec<u8> {
    let mut v = vec![];
    while len > 0 {
        // one push of m data bytes costs 1 + m (m <= 75), 2 + m (PUSHDATA1, m <= 255) or 3 + m (PUSHDATA2)
        let take = len.min(300);
        let rest = len - take;
        let (take, rest) = if rest > 0 && rest < 2 { (take - 2, rest + 2) } else { (take, rest) };
        if take <= 76 {
            let m = take - 1;
            v.push(m as u8);
            v.extend(std::iter::repeat_n(0xaau8, m));
        } else if take <= 257 {
            let m = take - 2;
            v.push(0x4c);
            v.push(m as u8);
            v.extend(std::iter::repeat_n(0xaau8, m));
        } else {
            let m = take - 3;
            v.push(0x4d);
            v.extend_from_slice(&(m as u16).to_le_bytes());
            v.extend(std::iter::repeat_n(0xaau8, m));
        }
        len = rest;
    }
    v
}

/// Number of data pushes in a push-only byte string (minimal pushes, OP_0, PUSHDATA1 / 2).
pub fn push_count(b: &[u8]) -> usize {
    let (mut i, mut n) = (0usize, 0usize);
    while i < b.len() {
        let op = b[i] as usize;
        i += match op {
            0 => 1,
            1..=75 => 1 + op,
            0x4c => 2 + b[i + 1] as usize,
            0x4d => 3 + u16::from_le_bytes([b[i + 1], b[i + 2]]) as usize,
            _ => panic!("not a push: {op:#x}"),
        };
        n += 1;
    }
    assert_eq!(i, b.len());
    n
}

/// A look-alike template: `pre` bytes of pushes, the state (pushes), then `suf` bytes that drop every pushed item and
/// end with OP_TRUE. It executes to true as a P2SH redeem script but is not the genuine template (another hash).
pub fn fake_rs(pre: usize, state: &[u8], suf: usize) -> Vec<u8> {
    use kaspa_txscript::opcodes::codes::{OpDrop, OpEndIf, OpFalse, OpIf, OpNop, OpTrue};
    let mut v = filler(pre);
    v.extend_from_slice(state);
    let drops = push_count(&v);
    assert!(suf > drops, "suffix too short for the look-alike");
    v.extend(std::iter::repeat_n(OpDrop, drops));
    let pad = suf - drops - 1;
    match pad {
        0 => {}
        1 | 2 => v.extend(std::iter::repeat_n(OpNop, pad)),
        _ => {
            v.extend([OpFalse, OpIf]);
            v.extend(filler(pad - 3));
            v.push(OpEndIf);
        }
    }
    v.push(OpTrue);
    assert_eq!(v.len(), pre + state.len() + suf);
    v
}

/// Input i becomes a planted UTXO of covenant `cov`: its sigscript pushes `rs`; `p2sh` = its script public key is
/// P2SH(rs) (rs must then execute to true), else `OP_DROP OP_TRUE` (the pushed bytes are never checked).
pub fn plant(tx: &mut Transaction, en: &mut [UtxoEntry], i: usize, cov: [u8; 32], rs: &[u8], p2sh: bool) {
    let spk = if p2sh { p2sh_spk(rs) } else { drop_true() };
    let e = &en[i];
    en[i] = UtxoEntry::new(e.amount, spk, e.block_daa_score, false, Some(Hash::from_bytes(cov)));
    tx.inputs[i].signature_script = push_data(rs);
}

pub type Patch<'a> = &'a dyn Fn(&mut Transaction, &mut Vec<UtxoEntry>);

/// The Schnorr signature of input i (SIGHASH_ALL unless the editor says otherwise) by `key`.
fn sig_of(tx: &Transaction, en: &[UtxoEntry], i: usize, key: [u8; 32], ht: u8) -> Vec<u8> {
    use kaspa_consensus_core::hashing::sighash::{calc_schnorr_signature_hash, SigHashReusedValuesUnsync};
    use kaspa_consensus_core::hashing::sighash_type::SigHashType;
    use kaspa_consensus_core::tx::MutableTransaction;
    let mt = MutableTransaction::with_entries(tx.clone(), en.to_vec());
    let reused = SigHashReusedValuesUnsync::new();
    let digest = calc_schnorr_signature_hash(&mt.as_verifiable(), i, SigHashType::from_u8(ht).unwrap(), &reused).as_bytes();
    let mut s = kob_protocol::tx::sign_digest(&keys()[&key], &digest).expect("sign");
    s[64] = ht;
    s
}

/// The finished transaction of `e` with `patch` applied, every signature redone over the patched transaction (a
/// signature commits to the spent outputs, so planting an input changes every digest).
pub fn finish_p(r: &Run, e: &Ed, patch: Patch) -> (Transaction, Vec<UtxoEntry>) {
    let (tx0, en0) = e.finish(r.subs);
    let (mut tx, mut en) = (tx0.clone(), en0.clone());
    patch(&mut tx, &mut en);
    for (i, p) in e.plans.iter().enumerate() {
        let Some(signer) = p.signer() else { continue };
        let key = e.signers.get(&i).map(|k| pk(*k)).unwrap_or(signer);
        let ht = e.sighash_types.get(&i).copied().unwrap_or(1);
        let (old, new) = (sig_of(&tx0, &en0, i, key, ht), sig_of(&tx, &en, i, key, ht));
        let ss = &mut tx.inputs[i].signature_script;
        if let Some(at) = ss.windows(65).position(|w| w == old.as_slice()) {
            ss[at..at + 65].copy_from_slice(&new);
        }
    }
    (tx, en)
}

pub fn exec_p(r: &Run, e: &Ed, patch: Patch) -> Vec<Result<u64, String>> {
    let (tx, en) = finish_p(r, e, patch);
    kob_protocol::verify::execute(&tx, &en, false).unwrap_or_else(|x| panic!("{} {}: tx-level failure {x}", e.name, r.pair))
}
/// [`Run::ok`] of a patched transaction.
pub fn ok_p(r: &Run, e: &Ed, patch: Patch) {
    let res = exec_p(r, e, patch);
    let failed: Vec<String> = res.iter().enumerate().filter_map(|(i, x)| x.as_ref().err().map(|m| format!("in[{i}]: {m}"))).collect();
    if failed.is_empty() {
        println!("POSITIVE {} {}  [PASS]", e.name, r.pair);
    } else if ablating() {
        println!("ABLATION-POS-FAIL {} {} ({})", e.name, r.pair, failed.join("; "));
    } else {
        panic!("{} {}: positive scenario rejected: {}", e.name, r.pair, failed.join("; "));
    }
}
/// [`Run::bad`] of a patched transaction.
pub fn bad_p(r: &Run, e: &Ed, expect: usize, patch: Patch) {
    let res = exec_p(r, e, patch);
    let x = &res[expect];
    if x.is_ok() && ablating() {
        println!("ABLATION-PASS {} {} all_inputs_ok={}", e.name, r.pair, res.iter().all(|r| r.is_ok()));
        return;
    }
    assert!(x.is_err(), "{} {}: expected input {expect} to REJECT but it passed", e.name, r.pair);
    let others: Vec<String> = res.iter().enumerate().filter(|(i, r)| *i != expect && r.is_err()).map(|(i, _)| i.to_string()).collect();
    println!(
        "NEGATIVE {} {}  [REJECTED at in[{expect}]: {}]{}",
        e.name,
        r.pair,
        x.as_ref().unwrap_err(),
        if others.is_empty() { String::new() } else { format!(" (also failing: {})", others.join(",")) }
    );
}

/// Runs `f` on each program pair of `mixes` with the templates under test.
pub fn suite(mixes: &[(TemplateId, TemplateId)], f: impl Fn(&Run, TemplateId, TemplateId)) {
    let subs = Subs::compile();
    for (pa, pb) in mixes {
        let r = Run { subs: &subs, pair: pair_name(*pa, *pb) };
        f(&r, *pa, *pb);
    }
}

/// Inventory fill of `n` base units of side `ask` at `PR` (rest), opened for editing; returns (ed, order input).
pub fn inv_ed(name: &str, s: PairState, pa: TemplateId, pb: TemplateId, n: i64) -> (Ed, usize) {
    let e = edb(name, inv(s, pa, pb, n));
    let i = oin(&e);
    (e, i)
}

// ---------------------------------------------------------------- positives

fn positives(r: &Run, pa: TemplateId, pb: TemplateId) {
    let ask = || ask0(pa, pb);
    let bid = || bid0(pa, pb);
    // routes through the KAS books
    r.ok(&edb("PP01 ask through the KAS books, rest", route_ask(ask(), pa, pb, 4)));
    r.ok(&edb("PP02 ask IOC, the rest returned at the custody index", route_ask(PairState { tif: TIF_IOC, ..ask() }, pa, pb, 4)));
    r.ok(&edb("PP03 ask sold out", route_ask(with_left(ask(), 4), pa, pb, 4)));
    r.ok(&edb("PP04 ask FOK, all of it", route_ask(PairState { tif: TIF_FOK, ..with_left(ask(), 4) }, pa, pb, 4)));
    r.ok(&edb("PP05 bid through the KAS books, rest", route_bid(bid(), pa, pb, 4)));
    r.ok(&edb("PP06 bid IOC, the escrow rest returned", route_bid(PairState { tif: TIF_IOC, ..bid() }, pa, pb, 4)));
    r.ok(&edb("PP07 bid done, the escrow slack returned", route_bid(with_left(bid(), 4), pa, pb, 4)));
    r.ok(&edb(
        "PP08 bid close: the exact escrow used up",
        route_bid(PairState { custody: 4 * WHOLE, ..with_left(bid(), 4) }, pa, pb, 4),
    ));
    // decay: a Dutch ask mid-way, a rising bid mid-way, both at their origin edge (t == origin == tx.daa) and at the end
    let dutch = PairState { price: RATE + 50, slope: 1, decay_step: 10, price_end: RATE - 50, active_from: NOW as i64 - 600, ..ask() };
    r.ok(&edb("PP09 Dutch ask mid-way", route_ask(dutch.clone(), pa, pb, 4)));
    let at_origin = PairState { active_from: NOW as i64, ..dutch.clone() };
    r.ok(&edb(
        "PP10 Dutch ask at its origin (t == origin == tx.daa)",
        route(vec![
            pair_leg(at_origin, pa, pb, X_ID, 70, 4),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 5),
        ]),
    ));
    r.ok(&edb("PP11 Dutch ask past its end (priceEnd)", route_ask(PairState { active_from: NOW as i64 - 5_000, ..dutch }, pa, pb, 4)));
    let rising =
        PairState { price: RATE - 50, slope: 1, decay_step: 10, price_end: RATE + 50, active_from: NOW as i64 - 500, ..bid() };
    let rising = PairState { custody: rising.bid_escrow(10 * WHOLE, 4).unwrap(), ..rising };
    r.ok(&edb("PP12 rising bid mid-way", route_bid(rising.clone(), pa, pb, 4)));
    let ro = PairState { active_from: NOW as i64, ..rising };
    let q = ro.s_out(4 * WHOLE, ro.price).unwrap();
    r.ok(&edb(
        "PP13 rising bid at its origin",
        route(vec![
            pair_leg(ro, pa, pb, Y_ID, 70, 4),
            kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 72, q),
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ]),
    ));
    // TWAP / DCA: interval (the input's sequence), maxFill, tip
    r.ok(&edb(
        "PP14 TWAP ask: interval, maxFill, tip",
        route_ask(PairState { interval: 100, max_fill: 4 * WHOLE, tip: 10_000, ..ask() }, pa, pb, 4),
    ));
    let dca = PairState { interval: 100, max_fill: 4 * WHOLE, tip: 10_000, ..bid() };
    r.ok(&edb("PP15 DCA bid: interval, maxFill, tip", route_bid(dca, pa, pb, 4)));
    // netting with opposite pair orders (no KAS-book leg): 1 x 1, 2 x 2, 1 x 3, 3 x 1, resting and closing
    r.ok(&edb("PP16 netting 1 x 1, both rest", netting(pa, pb, 1, 1, false)));
    r.ok(&edb("PP17 netting 1 x 1, both close", netting(pa, pb, 1, 1, true)));
    r.ok(&edb("PP18 netting 2 x 2, rest", netting(pa, pb, 2, 2, false)));
    r.ok(&edb("PP19 netting 2 x 2, close", netting(pa, pb, 2, 2, true)));
    r.ok(&edb("PP20 netting 1 ask x 3 bids", netting(pa, pb, 1, 3, false)));
    r.ok(&edb("PP21 netting 3 asks x 1 bid, close", netting(pa, pb, 3, 1, true)));
    // inventory fills: the matcher's own tokens are the counterparty
    r.ok(&edb("PP22 ask inventory fill (rounding price), rest", inv(side_pr(true, pa, pb), pa, pb, NR)));
    r.ok(&edb("PP23 bid inventory fill (rounding price), rest", inv(side_pr(false, pa, pb), pa, pb, NR)));
    r.ok(&edb("PP24 ask inventory fill, sold out", inv(left_units(side_pr(true, pa, pb), NR), pa, pb, NR)));
    r.ok(&edb("PP25 bid inventory fill, done", inv(left_units(side_pr(false, pa, pb), NR), pa, pb, NR)));
    // refunds (expiry, 90 days idle, IOC / FOK kill) and cancels
    r.ok(&ed("PP26 ask refund at expiry", refund_of(ask(), pa, pb, EXPIRY as u64, PV)));
    r.ok(&ed("PP27 bid refund at expiry", refund_of(bid(), pa, pb, EXPIRY as u64, PV)));
    let idle = PairState { expiry_daa: NO_EXPIRY, ..ask() };
    r.ok(&ed("PP28 refund after 90 days idle", refund_of(idle, pa, pb, (ODAA + MAX_IDLE) as u64, PV)));
    r.ok(&ed("PP29 IOC kill after 600 DAA", refund_of(PairState { tif: TIF_IOC, ..bid() }, pa, pb, (ODAA + 600) as u64, PV)));
    let late = PairState { tif: TIF_FOK, active_from: 5_000, ..ask() };
    r.ok(&ed("PP30 FOK kill 600 DAA after activeFrom", refund_of(late, pa, pb, 5_600, PV)));
    let strays = || vec![tutxo(pa, TOKEN_COV, 76, 3, OID, true), tutxo(pb, TOKEN_B, 77, 5, OID, true)];
    r.ok(&ed("PP31 ask cancel sweeping strays of both tokens", cancel_of(ask(), pa, pb, strays())));
    r.ok(&ed("PP32 bid cancel sweeping strays of both tokens", cancel_of(bid(), pa, pb, strays())));
}

#[test]
fn pair_positives() {
    suite(&family_mixes(), positives);
}

// ---------------------------------------------------------------- settlement: rounding, exact custody, C1 fake quotes

fn settlement(r: &Run, pa: TemplateId, pb: TemplateId) {
    let a = side_pr(true, pa, pb);
    let b = side_pr(false, pa, pb);
    // positives (the twins)
    let (e, _) = inv_ed("PP40 ask inventory fill at the exact ceil", a.clone(), pa, pb, NR);
    r.ok(&e);
    let (e, _) = inv_ed("PP41 bid inventory fill at the exact floor", b.clone(), pa, pb, NR);
    r.ok(&e);

    // NP01 the ASK maker paid one base unit short of ceil(n * p / scale) (the floor)
    let (mut e, i) = inv_ed("NP01 ask maker paid one base unit short (the floor, not the ceil)", a.clone(), pa, pb, NR);
    let t_out = e.arg_int(i, 5);
    e.set_arg(i, 5, Arg::Int(t_out - 1));
    add_amt(&mut e, i, -1);
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    add_amt(&mut e, m, 1);
    r.bad(&e, i);
    // NP02 the ASK releases one base unit of A more than n
    let (mut e, i) = inv_ed("NP02 ask releases n + 1 of A", a.clone(), pa, pb, NR);
    let s_out = e.arg_int(i, 4);
    e.set_arg(i, 4, Arg::Int(s_out + 1));
    let c = cin(&e, i);
    add_amt(&mut e, c, -1);
    let m = tok_out(&e, TOKEN_COV, pk(MATCHER));
    add_amt(&mut e, m, 1);
    set_cont_o(&mut e, |x| x.custody -= 1);
    r.bad(&e, i);
    // NP03 the BID pays one base unit MORE than its floor
    let (mut e, i) = inv_ed("NP03 bid pays one base unit more than its floor", b.clone(), pa, pb, NR);
    let s_out = e.arg_int(i, 4);
    e.set_arg(i, 4, Arg::Int(s_out + 1));
    let c = cin(&e, i);
    add_amt(&mut e, c, -1);
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    add_amt(&mut e, m, 1);
    set_cont_o(&mut e, |x| x.custody -= 1);
    r.bad(&e, i);
    // NP04 the BID pays one base unit LESS than its floor (a bid pays exactly its quote)
    let (mut e, i) = inv_ed("NP04 bid pays one base unit less than its floor", b.clone(), pa, pb, NR);
    let s_out = e.arg_int(i, 4);
    e.set_arg(i, 4, Arg::Int(s_out - 1));
    let c = cin(&e, i);
    add_amt(&mut e, c, 1);
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    add_amt(&mut e, m, -1);
    set_cont_o(&mut e, |x| x.custody += 1);
    r.bad(&e, i);
    // NP05 the BID receives one base unit of A short of n
    let (mut e, i) = inv_ed("NP05 bid receives n - 1 of A", b.clone(), pa, pb, NR);
    e.set_arg(i, 5, Arg::Int(NR - 1));
    add_amt(&mut e, i, -1);
    let m = tok_out(&e, TOKEN_COV, pk(MATCHER));
    add_amt(&mut e, m, 1);
    r.bad(&e, i);
    // NP06 / NP07 custody input holding one base unit more than `custody` (held != custody), the extra to the matcher
    for (id, s) in [("NP06 ask", a.clone()), ("NP07 bid", b.clone())] {
        let (mut e, i) =
            inv_ed(&format!("{id} custody input holds custody + 1 (the extra unit to the matcher)"), s.clone(), pa, pb, NR);
        let c = cin(&e, i);
        let st = in_tok(&e, c);
        set_in_tok(&mut e, c, st.with_amount(st.amount() + 1));
        let m = tok_out(&e, s_tok(s.is_ask()), pk(MATCHER));
        add_amt(&mut e, m, 1);
        r.bad(&e, i);
    }
    // NP08 a BID filled for more than its amountLeft (an IOC done fill of 5 whole A, amountLeft 4 whole A)
    let (mut e, i) = inv_ed("NP08 bid filled beyond its amountLeft", PairState { tif: TIF_IOC, ..bid0(pa, pb) }, pa, pb, 5 * WHOLE);
    restate(&mut e, i, |s| {
        s.tif = TIF_GTC;
        s.amount_left = 4 * WHOLE;
    });
    r.bad(&e, i);
    // NP09 (held by the custody rule) an ASK filled beyond its amountLeft: its custody cannot release it
    let (mut e, i) = inv_ed("NP09 ask filled beyond its amountLeft", PairState { tif: TIF_IOC, ..ask0(pa, pb) }, pa, pb, 5 * WHOLE);
    restate(&mut e, i, |s| {
        s.tif = TIF_GTC;
        s.amount_left = 4 * WHOLE;
    });
    r.bad(&e, i);
}

#[test]
fn pair_settlement() {
    suite(&mixed(), settlement);
}

/// The C1 fake quotes: every quote a pair order shows is takeable at that quote, or the order cannot be filled at all.
fn fake_quotes(r: &Run, pa: TemplateId, pb: TemplateId) {
    // NP10 an unfunded pair BID: its escrow one base unit below the quote of the fill (sold out, the matcher takes the
    // whole escrow)
    let b = left_units(side_pr(false, pa, pb), NR);
    let q = b.s_out(NR, PR).unwrap();
    let (mut e, i) = inv_ed("NP10 unfunded bid: escrow one unit below its quote", PairState { custody: q, ..b.clone() }, pa, pb, NR);
    restate(&mut e, i, |s| s.custody = q - 1);
    let c = cin(&e, i);
    let st = in_tok(&e, c);
    set_in_tok(&mut e, c, st.with_amount(q - 1));
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    add_amt(&mut e, m, -1);
    r.bad(&e, i);
    // NP11 a carrier wall: the order UTXO does not fund deliveryCarrier + tip; the matcher funds the delivery carrier
    for (id, ask) in [("NP11a ask", true), ("NP11b bid", false)] {
        let s = PairState { tip: 10_000, ..side_pr(ask, pa, pb) };
        let (mut e, i) =
            inv_ed(&format!("{id} carrier wall: the order UTXO does not fund its delivery carrier and tip"), s.clone(), pa, pb, NR);
        let tip_kas = s.tip_kas(NR).unwrap();
        let v = (s.delivery_carrier + tip_kas - 1) as u64;
        let k = cont_of(&e, OID).unwrap();
        let cv = e.value(k);
        let old = e.entries[i].amount;
        e.set_input_value(i, v);
        // the continuation keeps nothing; the change pays the difference
        e.set_value(k, 0);
        let c = change(&e);
        let vc = e.value(c);
        e.set_value(c, vc + cv - (old - v));
        r.bad(&e, i);
    }
    // NP12 an ASK whose custody is not its amountLeft (one unit less, one unit more): impossible to fill
    for (id, d) in [("NP12a", -1i64), ("NP12b", 1)] {
        let (mut e, i) = inv_ed(&format!("{id} ask custody = amountLeft {d:+}"), side_pr(true, pa, pb), pa, pb, NR);
        restate(&mut e, i, |s| s.custody += d);
        let c = cin(&e, i);
        let st = in_tok(&e, c);
        set_in_tok(&mut e, c, st.with_amount(st.amount() + d));
        add_amt(&mut e, c, d);
        set_cont_o(&mut e, |x| x.custody += d);
        r.bad(&e, i);
    }
    // NP13 a BID continuing with nothing left in its escrow (a partial fill that uses the escrow up)
    let b = side_pr(false, pa, pb);
    let q = b.s_out(NR, PR).unwrap();
    let (mut e, i) = inv_ed("NP13 bid rests with an empty escrow", b, pa, pb, NR);
    restate(&mut e, i, |s| s.custody = q);
    let c = cin(&e, i);
    let st = in_tok(&e, c);
    set_in_tok(&mut e, c, st.with_amount(q));
    e.unbind(c, MATCHER);
    set_cont_o(&mut e, |x| x.custody = 0);
    r.bad(&e, i);
}

#[test]
fn pair_fake_quotes() {
    suite(&mixed(), fake_quotes);
}

// ---------------------------------------------------------------- the custody (identity, codec), planted inputs

fn custody(r: &Run, pa: TemplateId, pb: TemplateId) {
    // ---- per family of S: owner, covenant marker, minter / borrowing
    for fam in [Family::Kcc20, Family::Kron] {
        let ask = side_with(fam, true, pa, pb);
        let sp = s_prog(ask, pa, pb);
        let f = ftag(sp);
        // NP20s two IOC orders of one maker name ONE custody (the second order's): the first order's custody stays out
        let twin = || PairState { tif: TIF_IOC, ..side0(ask, pa, pb) };
        let mut b = route(vec![pair_leg(twin(), pa, pb, OID, 70, 2), pair_leg(twin(), pa, pb, [0x83; 32], 80, 2)]);
        b.taker_tokens = vec![tutxo(t_prog(ask, pa, pb), t_tok(ask), 60, 20 * WHOLE, pk(MATCHER), false)];
        let mut e = edb(&format!("NP20s{f} two orders name one custody (the other order's)"), b);
        let (x1, x2) = (oin(&e), e.input_of_cov([0x83; 32]));
        let (c1, c2) = (cin(&e, x1), cin(&e, x2));
        e.set_arg(x1, 1, Arg::Int(c2 as i64));
        // the first custody leaves the transaction (its input becomes the matcher's KAS), its return output goes too, and
        // the matcher receives only the second order's release
        let ret1 = c1;
        let released = pst(&e, x1).s_out(2 * WHOLE, RATE).unwrap();
        e.unbind(ret1, MATCHER);
        e.make_p2pk(c1, MATCHER);
        let m = tok_out(&e, s_tok(ask), pk(MATCHER));
        add_amt(&mut e, m, -released);
        r.bad(&e, x1);
        // NP21s the custody marker: KCC-20 owner scheme / borrowing, KRON owner type
        let (mut e, i) =
            inv_ed(&format!("NP21s{f} custody of the order's id under another owner type"), side_pr(ask, pa, pb), pa, pb, NR);
        let c = cin(&e, i);
        let st = match in_tok(&e, c) {
            TokenState::Kcc20(k) => TokenState::Kcc20(Kcc20State { borrow_scheme: 1, ..k }),
            TokenState::Kron(k) => TokenState::Kron(KronState { id_type: 3, ..k }),
        };
        set_in_tok(&mut e, c, st);
        r.bad(&e, i);
        if fam == Family::Kron {
            // NP22sr a minter custody
            let (mut e, i) = inv_ed("NP22sr KRON custody flagged as a minter", side_pr(ask, pa, pb), pa, pb, NR);
            let c = cin(&e, i);
            let TokenState::Kron(k) = in_tok(&e, c) else { unreachable!() };
            set_in_tok(&mut e, c, TokenState::Kron(KronState { is_minter: 1, ..k }));
            r.bad(&e, i);
        } else {
            // NP22sk hostile sFamily 3 (read as KCC-20)
            let (mut e, i) = inv_ed("NP22sk hostile sFamily 3", side_pr(ask, pa, pb), pa, pb, NR);
            restate(&mut e, i, |s| s.s_family = 3);
            r.bad(&e, i);
            // NP23tk hostile tFamily 3 (read as KCC-20)
            let tk_ask = side_with(Family::Kcc20, false, pa, pb);
            let (mut e, i) = inv_ed("NP23tk hostile tFamily 3", side_pr(tk_ask, pa, pb), pa, pb, NR);
            restate(&mut e, i, |s| s.t_family = 3);
            r.bad(&e, i);
        }
    }

    // ---- generic (both families through the two mixed pairs): the custody's token, P2SH, template; a zero custody
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let sold = || closing(ask, pa, pb);
        let sp = s_prog(ask, pa, pb);
        let stok = s_tok(ask);
        // NP24 the custody is a UTXO of another token C (same program) owned by the order: sold out, the C to the matcher
        let (mut e, i) = inv_ed(&format!("NP24{side} custody of another token owned by the order"), sold(), pa, pb, NR);
        let c = cin(&e, i);
        let en = &e.entries[c];
        e.entries[c] =
            UtxoEntry::new(en.amount, en.script_public_key.clone(), en.block_daa_score, false, Some(Hash::from_bytes(TOKEN_C)));
        e.programs.remove(&stok);
        e.programs.insert(TOKEN_C, sp);
        let m = tok_out(&e, stok, pk(MATCHER));
        let st = e.out_state(m);
        e.tok_out.insert(m, (TOKEN_C, st));
        r.bad(&e, i);
        // NP25 / NP26 a planted custody (a UTXO carrying S's covenant id that is no token of S): sold out, no S anywhere
        for (id, p2sh) in [("NP25", false), ("NP26", true)] {
            let what = if p2sh { "P2SH of a look-alike template" } else { "not P2SH (its pushed redeem script is never run)" };
            let (mut e, i) = inv_ed(&format!("{id}{side} planted custody: {what}"), sold(), pa, pb, NR);
            let c = cin(&e, i);
            let m = tok_out(&e, stok, pk(MATCHER));
            e.unbind(m, MATCHER);
            e.programs.remove(&stok);
            let state = in_tok(&e, c).encode();
            let tpl = token_template(sp);
            let rs = if p2sh {
                fake_rs(tpl.prefix.len(), &state, tpl.suffix.len())
            } else {
                [tpl.prefix.as_slice(), &state, tpl.suffix.as_slice()].concat()
            };
            e.make_p2pk(c, 30);
            bad_p(r, &e, i, &|tx, en| plant(tx, en, c, stok, &rs, p2sh));
        }
        // NP27 a refund of an order whose custody is 0: output i is not pinned (the keeper takes the order's KAS; the
        // empty custody goes to the keeper too)
        let mut e = ed(
            &format!("NP27{side} refund of a zero custody: output i to the keeper"),
            refund_of(side0(ask, pa, pb), pa, pb, EXPIRY as u64, PV),
        );
        let i = oin(&e);
        restate(&mut e, i, |x| x.custody = 0);
        let c = cin(&e, i);
        let st = in_tok(&e, c);
        set_in_tok(&mut e, c, st.with_amount(0));
        let st0 = e.out_state(i);
        let v = e.value(i);
        e.unbind(i, KEEPER);
        e.set_value(i, v);
        e.add_token_output(stok, st0.with_amount(0).with_user_owner(pk(KEEPER)), 0);
        r.bad(&e, i);
    }
}

#[test]
fn pair_custody() {
    suite(&mixed(), custody);
}

// ---------------------------------------------------------------- strays of both tokens on every path

fn strays(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let (sp, tp) = (s_prog(ask, pa, pb), t_prog(ask, pa, pb));
        let (stok, ttok) = (s_tok(ask), t_tok(ask));
        // the three fill paths: rest, IOC return, close
        let paths: [(&str, PairState); 3] = [
            ("rest", side_pr(ask, pa, pb)),
            ("ioc", PairState { tif: TIF_IOC, ..side_pr(ask, pa, pb) }),
            ("close", left_units(side_pr(ask, pa, pb), NR)),
        ];
        for (k, (path, s)) in paths.into_iter().enumerate() {
            let s = if !ask && path == "close" { PairState { custody: s.s_out(NR, PR).unwrap(), ..s } } else { s };
            // NP30 a stray of S owned by the order rides along to the matcher
            let (mut e, i) = inv_ed(&format!("NP30{side}{k} S stray of the order in a fill ({path})"), s.clone(), pa, pb, NR);
            add_stray(&mut e, stok, sp, OID, 3, 99);
            r.bad(&e, i);
            // NP31 a stray of T owned by the order rides along to the matcher
            let (mut e, i) = inv_ed(&format!("NP31{side}{k} T stray of the order in a fill ({path})"), s, pa, pb, NR);
            add_stray(&mut e, ttok, tp, OID, 3, 99);
            r.bad(&e, i);
        }
        // refunds: NP32 an S stray, NP33 a T stray (any T input), both to the keeper
        let mut e =
            ed(&format!("NP32{side} S stray of the order in a refund"), refund_of(side0(ask, pa, pb), pa, pb, EXPIRY as u64, PV));
        add_stray(&mut e, stok, sp, OID, 3, 99);
        r.bad(&e, 0);
        let mut e =
            ed(&format!("NP33{side} T stray of the order in a refund"), refund_of(side0(ask, pa, pb), pa, pb, EXPIRY as u64, PV));
        add_stray(&mut e, ttok, tp, OID, 3, 99);
        r.bad(&e, 0);
    }
    // the unrolled scan slot by slot, on the side whose T is the KCC-20 token (8 inputs per token): NP34t<k> the order's
    // T stray at scan slot k (slot 0: the matcher's T inventory input itself owned by the order; slot k >= 1: k - 1
    // key-held units of the matcher in front of it), so every slot line of noStrays is load-bearing; NP35t the stray as a
    // 9th T input, beyond the scan (the KCC-20 program refuses a 9th input itself: with the bound removed only the
    // order's input flips). PP34t: the 8-input shape with a key-held unit at slot 7.
    let ask = side_with(Family::Kcc20, false, pa, pb);
    let s = side_pr(ask, pa, pb);
    let (mut e, _) = inv_ed("PP34t eight T inputs (every scan slot), no stray", s.clone(), pa, pb, NR);
    stray_at_slot(&mut e, t_tok(ask), OID, 7, false);
    r.ok(&e);
    for k in 0..=8usize {
        let name = if k < 8 {
            format!("NP34t{k} T stray of the order at scan slot {k}")
        } else {
            "NP35t T stray of the order as a 9th T input (beyond the scan)".into()
        };
        let (mut e, i) = inv_ed(&name, s.clone(), pa, pb, NR);
        stray_at_slot(&mut e, t_tok(ask), OID, k, true);
        r.bad(&e, i);
    }
}

#[test]
fn pair_strays() {
    suite(&mixed(), strays);
}

// ---------------------------------------------------------------- output aliasing between several orders

fn aliasing(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let ttok = t_tok(ask);
        // NP40 two orders of one maker: the second order's delivery paid to the matcher
        let twin = || side0(ask, pa, pb);
        let mut b = route(vec![pair_leg(twin(), pa, pb, OID, 70, 2), pair_leg(twin(), pa, pb, [0x83; 32], 80, 2)]);
        b.taker_tokens = vec![tutxo(t_prog(ask, pa, pb), ttok, 60, 20 * WHOLE, pk(MATCHER), false)];
        let mut e = edb(&format!("NP40{side} one delivery for two orders of one maker (the second slot pays the matcher)"), b);
        let x2 = e.input_of_cov([0x83; 32]);
        let st = e.out_state(x2);
        e.set_out_state(x2, st.with_user_owner(pk(MATCHER)));
        r.bad(&e, x2);
        // NP41 the rest of S moved off the custody index (swapped with the matcher's S output)
        let (mut e, i) = inv_ed(&format!("NP41{side} custody rest at another index"), side_pr(ask, pa, pb), pa, pb, NR);
        let c = cin(&e, i);
        let m = tok_out(&e, s_tok(ask), pk(MATCHER));
        e.swap_outputs(c, m);
        r.bad(&e, i);
        // NP42 two continuations: a second output bound to the order's covenant id (a forged UTXO of the id)
        let (mut e, i) = inv_ed(&format!("NP42{side} a second output bound to the order's id"), side_pr(ask, pa, pb), pa, pb, NR);
        forge_bound(&mut e, OID, i, KAS);
        r.bad(&e, i);
        // NP43 no continuation on a partial fill
        let (mut e, i) = inv_ed(&format!("NP43{side} partial fill without its continuation"), side_pr(ask, pa, pb), pa, pb, NR);
        let k = cont_of(&e, OID).unwrap();
        e.unbind(k, MATCHER);
        r.bad(&e, i);
        // NP44 a terminating fill (IOC) that leaves an output bound to the order's id
        let ioc = PairState { tif: TIF_IOC, ..side_pr(ask, pa, pb) };
        let (mut e, i) = inv_ed(&format!("NP44{side} IOC fill leaving an output bound to the order's id"), ioc, pa, pb, NR);
        forge_bound(&mut e, OID, i, KAS);
        r.bad(&e, i);
        // NP45 a sibling UTXO of the order's covenant id spent beside it
        let (mut e, i) =
            inv_ed(&format!("NP45{side} a sibling UTXO of the order's covenant id in the fill"), side_pr(ask, pa, pb), pa, pb, NR);
        let j = e.add_p2pk_input(MATCHER, 0xea);
        let en = &e.entries[j];
        e.entries[j] = UtxoEntry::new(en.amount, en.script_public_key.clone(), en.block_daa_score, false, Some(Hash::from_bytes(OID)));
        r.bad(&e, i);
    }
}

#[test]
fn pair_aliasing() {
    suite(&mixed(), aliasing);
}

// ---------------------------------------------------------------- order parameters: quantity, time, decay, hostile fields

/// The maker's B of an ASK inventory fill set to `want` (the difference to / from the matcher's B).
fn set_t_out(e: &mut Ed, i: usize, want: i64) {
    let have = e.arg_int(i, 5);
    e.set_arg(i, 5, Arg::Int(want));
    add_amt(e, i, want - have);
    let m = tok_out(e, TOKEN_B, pk(MATCHER));
    add_amt(e, m, have - want);
}

fn params(r: &Run, pa: TemplateId, pb: TemplateId) {
    let a = || side_pr(true, pa, pb);
    let b = || side_pr(false, pa, pb);
    // NP50 below minFill (partial)
    let (mut e, i) = inv_ed("NP50 partial fill below minFill", a(), pa, pb, NR);
    restate(&mut e, i, |s| s.min_fill = NR + 1);
    r.bad(&e, i);
    // NP51 above maxFill
    let (mut e, i) = inv_ed("NP51 fill above maxFill", a(), pa, pb, NR);
    restate(&mut e, i, |s| s.max_fill = NR - 1);
    r.bad(&e, i);
    // NP52 TWAP: the interval not elapsed (the input's sequence one DAA short)
    let (mut e, i) = inv_ed("NP52 TWAP fill before its interval", PairState { interval: 100, ..a() }, pa, pb, NR);
    e.tx.inputs[i].sequence = 99;
    r.bad(&e, i);
    // NP53 FOK filled partially (an IOC fill turned FOK)
    let (mut e, i) = inv_ed("NP53 FOK filled partially", PairState { tif: TIF_IOC, ..a() }, pa, pb, NR);
    restate(&mut e, i, |s| s.tif = TIF_FOK);
    r.bad(&e, i);
    // NP54 before activeFrom
    let (mut e, i) = inv_ed("NP54 fill before activeFrom", b(), pa, pb, NR);
    restate(&mut e, i, |s| s.active_from = NOW as i64 + 1);
    r.bad(&e, i);
    // NP55 n < 0 (a negative fill under a hostile minFill)
    let (mut e, i) = inv_ed("NP55 negative fill amount", a(), pa, pb, NR);
    restate(&mut e, i, |s| s.min_fill = -5);
    e.set_arg(i, 0, nb(-1));
    r.bad(&e, i);
    // ---- decay: a Dutch ask from PR + 50 falling one unit per 10 DAA from NOW - 600 (p(NOW) = PR - 10)
    let dutch = PairState { price: PR + 50, slope: 1, decay_step: 10, price_end: PR - 50, active_from: NOW as i64 - 600, ..a() };
    let (e, _) = inv_ed("PP50 Dutch ask inventory fill at t = tx.daa", dutch.clone(), pa, pb, NR);
    r.ok(&e);
    // the maker's B at another decay time t (ceil at p(t))
    let at_t = |name: &str, t: i64| {
        let (mut e, i) = inv_ed(name, dutch.clone(), pa, pb, NR);
        let want = dutch.t_out_min(NR, dutch.price_at(t, ODAA).unwrap()).unwrap();
        e.set_arg(i, 3, Arg::Int(t));
        set_t_out(&mut e, i, want);
        (e, i)
    };
    // NP56 t beyond the transaction's DAA (an overstated t: a lower price for the maker)
    let (e, i) = at_t("NP56 decay time t after tx.daa (lower price)", NOW as i64 + 300);
    r.bad(&e, i);
    // NP57 t before the origin (paid at the price the covenant computes for it)
    let (e, i) = at_t("NP57 decay time t before the origin", NOW as i64 - 610);
    r.bad(&e, i);
    // NP58 hostile negative slope / NP59 negative decayStep (the price moves up by 60; the maker paid that)
    for (id, neg_slope) in [("NP58 hostile negative slope", true), ("NP59 hostile negative decayStep", false)] {
        let (mut e, i) = inv_ed(id, dutch.clone(), pa, pb, NR);
        restate(&mut e, i, move |x| {
            if neg_slope {
                x.slope = -1;
            } else {
                x.decay_step = -10;
            }
        });
        let want = dutch.t_out_min(NR, dutch.price + 60).unwrap();
        set_t_out(&mut e, i, want);
        r.bad(&e, i);
    }
    // NP65 a TWAP Dutch ask whose slice opened after activeFrom: the decay counts from the slice (UTXO DAA + interval),
    // not from activeFrom (the maker paid at the price from activeFrom: 30 units lower)
    let twap = PairState { interval: NOW as i64 - 300 - ODAA, ..dutch.clone() };
    let (e, _) = inv_ed("PP51 TWAP Dutch ask, the decay from the slice opening", twap.clone(), pa, pb, NR);
    r.ok(&e);
    let (mut e, i) = inv_ed("NP65 TWAP Dutch ask priced from activeFrom instead of its slice", twap.clone(), pa, pb, NR);
    set_t_out(&mut e, i, twap.t_out_min(NR, twap.price - 60).unwrap());
    r.bad(&e, i);
    // NP66 a Dutch ask past its end paid below priceEnd (the decay continued)
    let late = PairState { active_from: NOW as i64 - 1_500, ..dutch.clone() };
    let (mut e, i) = inv_ed("NP66 Dutch ask past its end paid below priceEnd", late.clone(), pa, pb, NR);
    set_t_out(&mut e, i, late.t_out_min(NR, late.price - 150).unwrap());
    r.bad(&e, i);
    // NP67 a rising bid past its end paying above priceEnd (the maker overpays)
    let rising = PairState { price: PR - 50, slope: 1, decay_step: 10, price_end: PR + 50, active_from: NOW as i64 - 1_500, ..b() };
    let rising = PairState { custody: rising.bid_escrow(10 * WHOLE, 4).unwrap() + 100, ..rising };
    let (mut e, i) = inv_ed("NP67 rising bid past its end paying above priceEnd", rising.clone(), pa, pb, NR);
    let q = e.arg_int(i, 4);
    let q2 = rising.s_out(NR, rising.price + 150).unwrap();
    e.set_arg(i, 4, Arg::Int(q2));
    let c = cin(&e, i);
    add_amt(&mut e, c, q - q2);
    set_cont_o(&mut e, |x| x.custody += q - q2);
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    add_amt(&mut e, m, q2 - q);
    r.bad(&e, i);
    // NP60 a BID at price 0 (hostile: n of A for nothing, the escrow keeps everything)
    let (mut e, i) = inv_ed("NP60 bid at price 0", b(), pa, pb, NR);
    let q = e.arg_int(i, 4);
    restate(&mut e, i, |s| s.price = 0);
    e.set_arg(i, 4, Arg::Int(0));
    let c = cin(&e, i);
    add_amt(&mut e, c, q);
    set_cont_o(&mut e, |x| x.custody += q);
    let m = tok_out(&e, TOKEN_B, pk(MATCHER));
    e.unbind(m, MATCHER);
    r.bad(&e, i);
    // NP61 hostile negative tip (the matcher funds the negative "tip" into the continuation)
    let (mut e, i) = inv_ed("NP61 hostile negative tip", a(), pa, pb, NR);
    restate(&mut e, i, |s| s.tip = -10);
    let k = cont_of(&e, OID).unwrap();
    fund(&mut e, k, 41);
    r.bad(&e, i);
    // NP62 hostile negative deliveryCarrier
    let (mut e, i) = inv_ed("NP62 hostile negative deliveryCarrier", a(), pa, pb, NR);
    let dc = pst(&e, i).delivery_carrier;
    restate(&mut e, i, |s| s.delivery_carrier = -1);
    let k = cont_of(&e, OID).unwrap();
    fund(&mut e, k, dc + 1);
    r.bad(&e, i);
    // NP63 hostile side 3 (filled as a bid)
    let (mut e, i) = inv_ed("NP63 hostile side 3", b(), pa, pb, NR);
    restate(&mut e, i, |s| s.side = 3);
    r.bad(&e, i);
    // NP64a a maker's ASK with a negative scale of A (sScale -1000): the ceil of its quote is negative, so it would sell
    // NR of A for one base unit of B (PP64a: the honest twin)
    let (e, _) = inv_ed("PP64a ask inventory fill (the twin of NP64a)", a(), pa, pb, NR);
    r.ok(&e);
    let (mut e, i) = inv_ed("NP64a ask with a negative scale of A, filled for one unit of B", a(), pa, pb, NR);
    restate(&mut e, i, |s| s.s_scale = -1_000);
    set_cont_o(&mut e, |s| s.s_scale = -1_000);
    set_t_out(&mut e, i, 1);
    r.bad(&e, i);
    // NP64b a BID with a negative scale of A (tScale -1000): its floor quote is negative, so the fill "pays" a negative
    // amount (the matcher tops the escrow up from a key-held B input of its own) (PP64b: the honest twin)
    let (e, _) = inv_ed("PP64b bid inventory fill (the twin of NP64b)", b(), pa, pb, NR);
    r.ok(&e);
    let (mut e, i) = inv_ed("NP64b bid with a negative scale of A, paying a negative quote", b(), pa, pb, NR);
    let old = e.arg_int(i, 4);
    let new = quote_trunc(NR, PR, -1_000, 0);
    assert!(new < 0, "NP64b: the quote at scale -1000 is negative");
    let d = old - new;
    restate(&mut e, i, |s| s.t_scale = -1_000);
    set_cont_o(&mut e, |s| {
        s.t_scale = -1_000;
        s.custody += d;
    });
    e.set_arg(i, 4, Arg::Int(new));
    let c = cin(&e, i);
    add_amt(&mut e, c, d);
    let bp = s_prog(false, pa, pb);
    e.add_token_input(utxo(0xd1, CARRIER, 1_000, Some(TOKEN_B)), user(bp, d, MATCHER), Witness::P2pk(pk(MATCHER)));
    r.bad(&e, i);
}

/// KobPair's quoteOf(n, r, d, c) with the engine's truncating division (also for a negative d).
fn quote_trunc(n: i64, r: i64, d: i64, c: i64) -> i64 {
    let (m, q) = (n % d, n / d);
    q * r + m * (r / d) + (m * (r % d) + c) / d
}

#[test]
fn pair_params() {
    suite(&mixed(), params);
}

// ---------------------------------------------------------------- refunds and cancels

fn lifecycle(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let s0 = || side0(ask, pa, pb);
        let stok = s_tok(ask);
        // refund timing: expiry, the 90-day idle bound, the IOC kill (from the UTXO DAA and from activeFrom)
        let early = |id: &str, what: &str, s: PairState, due: i64| {
            let mut e = ed(&format!("{id}{side} refund one DAA before {what}"), refund_of(s, pa, pb, due as u64, PV));
            e.tx.lock_time = (due - 1) as u64;
            r.bad(&e, 0);
        };
        early("NP70", "expiry", PairState { expiry_daa: 5_000_000, ..s0() }, 5_000_000);
        early("NP71", "90 days idle", PairState { expiry_daa: NO_EXPIRY, ..s0() }, ODAA + MAX_IDLE);
        early("NP72", "the IOC kill (600 DAA)", PairState { tif: TIF_IOC, ..s0() }, ODAA + 600);
        early("NP73", "the FOK kill counted from activeFrom", PairState { tif: TIF_FOK, active_from: 5_000, ..s0() }, 5_600);
        // NP74 the keeper takes one sompi more than refundTip
        let mut e =
            ed(&format!("NP74{side} refund: the keeper takes more than refundTip"), refund_of(s0(), pa, pb, EXPIRY as u64, PV));
        let rt = s0().refund_tip;
        let v = e.value(0);
        e.set_value(0, v - rt as u64 - 1);
        let c = change(&e);
        let vc = e.value(c);
        e.set_value(c, vc + rt as u64 + 1);
        r.bad(&e, 0);
        // NP76 the custody refunded to the keeper
        let mut e = ed(&format!("NP76{side} refund: the custody to the keeper"), refund_of(s0(), pa, pb, EXPIRY as u64, PV));
        let st = e.out_state(0);
        e.set_out_state(0, st.with_user_owner(pk(KEEPER)));
        r.bad(&e, 0);
        // NP77 a refund that leaves an output bound to the order's id (a forged owner of its strays)
        let mut e =
            ed(&format!("NP77{side} refund leaving an output bound to the order's id"), refund_of(s0(), pa, pb, EXPIRY as u64, PV));
        forge_bound(&mut e, OID, 0, KAS);
        r.bad(&e, 0);
        // NP78 refund: the custody returned at another index
        let mut e = ed(&format!("NP78{side} refund: the custody returned off output i"), refund_of(s0(), pa, pb, EXPIRY as u64, PV));
        let st = e.out_state(0);
        let v = e.value(0);
        e.unbind(0, MAKER_A);
        e.set_value(0, v);
        e.programs.entry(stok).or_insert(s_prog(ask, pa, pb));
        let k = e.add_token_output(stok, st, 0);
        fund(&mut e, k, 0);
        r.bad(&e, 0);
        // cancels: NP79 a non-ALL signature hash type, NP80 another key
        let mut e = ed(&format!("NP79{side} cancel with a non-ALL signature hash type"), cancel_of(s0(), pa, pb, vec![]));
        e.sighash_types.insert(0, 2);
        r.bad(&e, 0);
        let mut e = ed(&format!("NP80{side} cancel signed by another key"), cancel_of(s0(), pa, pb, vec![]));
        e.sign_as(0, MAKER_B);
        r.bad(&e, 0);
    }
}

#[test]
fn pair_lifecycle() {
    suite(&mixed(), lifecycle);
}

// ---------------------------------------------------------------- the maker's T output and the S outputs

fn outputs(r: &Run, pa: TemplateId, pb: TemplateId) {
    // ---- per family of T
    for fam in [Family::Kcc20, Family::Kron] {
        let ask = side_with(fam, false, pa, pb);
        let tp = t_prog(ask, pa, pb);
        let f = ftag(tp);
        let honest = |id: &str| inv_ed(id, side_pr(ask, pa, pb), pa, pb, NR);
        // NP90t the delivery owned by the matcher
        let (mut e, i) = honest(&format!("NP90t{f} the maker's T delivered to the matcher"));
        let st = e.out_state(i);
        e.set_out_state(i, st.with_user_owner(pk(MATCHER)));
        r.bad(&e, i);
        // NP91t the delivery covenant-owned (KCC-20 scheme 0x04 / KRON id_type 2) by the maker's key bytes
        let (mut e, i) = honest(&format!("NP91t{f} the maker's T under a covenant owner type"));
        let st = match e.out_state(i) {
            TokenState::Kcc20(k) => TokenState::Kcc20(Kcc20State { owner_scheme: SCHEME_COVID, ..k }),
            TokenState::Kron(k) => TokenState::Kron(KronState { id_type: 2, ..k }),
        };
        e.set_out_state(i, st);
        r.bad(&e, i);
        if fam == Family::Kcc20 {
            // NP92tk another extension commitment; NP93tk borrowing enabled
            let (mut e, i) = honest("NP92tk the maker's T with another extension commitment");
            let TokenState::Kcc20(k) = e.out_state(i) else { unreachable!() };
            e.set_out_state(i, TokenState::Kcc20(Kcc20State { extension_commitment: [0x33; 32], ..k }));
            r.bad(&e, i);
            let (mut e, i) = honest("NP93tk the maker's T with borrowing enabled");
            let TokenState::Kcc20(k) = e.out_state(i) else { unreachable!() };
            e.set_out_state(i, TokenState::Kcc20(Kcc20State { borrow_scheme: 1, ..k }));
            r.bad(&e, i);
        } else {
            // NP92tr the maker's T flagged as a minter
            let (mut e, i) = honest("NP92tr the maker's T flagged as a minter");
            let TokenState::Kron(k) = e.out_state(i) else { unreachable!() };
            e.set_out_state(i, TokenState::Kron(KronState { is_minter: 1, ..k }));
            r.bad(&e, i);
        }
    }
    // ---- generic
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let (sp, tp) = (s_prog(ask, pa, pb), t_prog(ask, pa, pb));
        let (stok, ttok) = (s_tok(ask), t_tok(ask));
        let honest = |id: &str| inv_ed(&sided(id, side), side_pr(ask, pa, pb), pa, pb, NR);
        // NP94 the delivery output carries the token's script but no covenant binding; the tokens go to the matcher
        let (mut e, i) = honest("NP94 delivery without the T covenant binding");
        let st = e.out_state(i);
        let spk = st.spk_with(token_template(tp));
        e.tok_out.remove(&i);
        e.tx.outputs[i].covenant = None;
        e.tx.outputs[i].script_public_key = spk;
        let m = tok_out(&e, ttok, pk(MATCHER));
        add_amt(&mut e, m, st.amount());
        r.bad(&e, i);
        // NP95 the T template source planted: a UTXO with T's covenant id that is no T token (a look-alike template); the
        // maker gets an output of that look-alike script bound to T's id
        let (mut e, i) = honest("NP95 T template source planted (look-alike T output)");
        let j = e.add_p2pk_input(30, 0xeb);
        e.set_arg(i, 2, Arg::Int(j as i64));
        let st = e.out_state(i);
        let ttpl = token_template(tp);
        let rs = fake_rs(ttpl.prefix.len(), &st.encode(), ttpl.suffix.len());
        let m = tok_out(&e, ttok, pk(MATCHER));
        add_amt(&mut e, m, st.amount());
        e.tok_out.remove(&i);
        let delivery = p2sh_spk(&rs);
        bad_p(r, &e, i, &|tx, en| {
            plant(tx, en, j, ttok, &rs, true);
            tx.outputs[i].script_public_key = delivery.clone();
            tx.outputs[i].covenant =
                Some(kaspa_consensus_core::tx::CovenantBinding { authorizing_input: j as u16, covenant_id: Hash::from_bytes(ttok) });
        });
        // NP96 (held by the template hash) the T template read from a token input of another program (the S custody)
        let (mut e, i) = honest("NP96 T template source at an input of another program");
        let c = cin(&e, i);
        e.set_arg(i, 2, Arg::Int(c as i64));
        r.bad(&e, i);
        // S outputs: NP97 the rest owned by the matcher's key; NP98 the IOC return to the matcher; NP99 the rest one unit
        // short (the unit to the matcher)
        let (mut e, i) = honest("NP97 custody rest owned by the matcher");
        let c = cin(&e, i);
        let st = e.out_state(c);
        e.set_out_state(c, st.with_user_owner(pk(MATCHER)));
        r.bad(&e, i);
        let (mut e, i) =
            inv_ed(&format!("NP98{side} IOC return to the matcher"), PairState { tif: TIF_IOC, ..side_pr(ask, pa, pb) }, pa, pb, NR);
        let c = cin(&e, i);
        let st = e.out_state(c);
        e.set_out_state(c, st.with_user_owner(pk(MATCHER)));
        r.bad(&e, i);
        let (mut e, i) = honest("NP99 custody rest one unit short");
        let c = cin(&e, i);
        add_amt(&mut e, c, -1);
        let m = tok_out(&e, stok, pk(MATCHER));
        add_amt(&mut e, m, 1);
        r.bad(&e, i);
        // NP100 the custody rest without the S covenant binding (its script kept; the tokens to the matcher)
        let (mut e, i) = honest("NP100 custody rest without the S covenant binding");
        let c = cin(&e, i);
        let st = e.out_state(c);
        let spk = st.spk_with(token_template(sp));
        e.tok_out.remove(&c);
        e.tx.outputs[c].covenant = None;
        e.tx.outputs[c].script_public_key = spk;
        let m = tok_out(&e, stok, pk(MATCHER));
        add_amt(&mut e, m, st.amount());
        r.bad(&e, i);
    }
}

#[test]
fn pair_outputs() {
    suite(&mixed(), outputs);
}

// ---------------------------------------------------------------- continuation and carriers

fn carriers(r: &Run, pa: TemplateId, pb: TemplateId) {
    for ask in [true, false] {
        let side = if ask { "a" } else { "b" };
        let rest = |id: &str| inv_ed(&sided(id, side), PairState { tip: 10_000, ..side_pr(ask, pa, pb) }, pa, pb, NR);
        // NP110 the continuation keeps amountLeft (n not taken off); NP111 its custody field not reduced
        let (mut e, i) = rest("NP110 continuation keeps amountLeft");
        let k = cont_of(&e, OID).unwrap();
        set_cont(&mut e, k, |x| x.amount_left += NR);
        r.bad(&e, i);
        let (mut e, i) = rest("NP111 continuation custody field off by one");
        let k = cont_of(&e, OID).unwrap();
        set_cont(&mut e, k, |x| x.custody += 1);
        r.bad(&e, i);
        // NP112 the continuation drained by one sompi; NP113 the custody rest's carrier; NP114 the delivery carrier
        let (mut e, i) = rest("NP112 continuation drained by one sompi");
        let k = cont_of(&e, OID).unwrap();
        fund(&mut e, k, -1);
        r.bad(&e, i);
        let (mut e, i) = rest("NP113 custody rest drained of its carrier");
        let c = cin(&e, i);
        fund(&mut e, c, -1);
        r.bad(&e, i);
        let (mut e, i) = rest("NP114 delivery carrier short");
        fund(&mut e, i, -1);
        r.bad(&e, i);
        // IOC: NP115 the return's carrier, NP116 the maker's KAS (order value minus the tip)
        let ioc = |id: &str| inv_ed(&sided(id, side), PairState { tif: TIF_IOC, tip: 10_000, ..side_pr(ask, pa, pb) }, pa, pb, NR);
        let (mut e, i) = ioc("NP115 IOC return drained of its carrier");
        let c = cin(&e, i);
        fund(&mut e, c, -1);
        r.bad(&e, i);
        let (mut e, i) = ioc("NP116 IOC: the maker's KAS one sompi short");
        fund(&mut e, i, -1);
        r.bad(&e, i);
        // NP118 the filler takes the ceil of the tip (tip 10,001 sompi per whole A: floor and ceil differ), one sompi out
        // of the continuation
        let (mut e, i) = inv_ed(
            &sided("NP118 the filler takes the ceil of the KAS tip", side),
            PairState { tip: 10_001, ..side_pr(ask, pa, pb) },
            pa,
            pb,
            NR,
        );
        let k = cont_of(&e, OID).unwrap();
        fund(&mut e, k, -1);
        r.bad(&e, i);
        // NP117 sold out: every carrier back to the maker but the tip
        let mut s = left_units(PairState { tip: 10_000, ..side_pr(ask, pa, pb) }, NR);
        if !ask {
            s.custody = s.s_out(NR, PR).unwrap();
        }
        let (mut e, i) = inv_ed(&format!("NP117{side} sold out: the maker's carriers one sompi short"), s, pa, pb, NR);
        fund(&mut e, i, -1);
        r.bad(&e, i);
    }
}

#[test]
fn pair_carriers() {
    suite(&mixed(), carriers);
}

// ---------------------------------------------------------------- limits: KRON amounts and witness indexes, overflow

/// The most a KRON token UTXO holds.
pub const KRON_MAX: i64 = 1_000_000_000;

/// Inserts `k` plain inputs (the matcher's) and `k` plain outputs in front of everything: every input and output index
/// moves up by k (pair-order arguments that name inputs, bindings, token outputs follow).
pub fn shift_front(e: &mut Ed, k: usize) {
    use kaspa_consensus_core::tx::{TransactionInput, TransactionOutpoint, TransactionOutput};
    for j in 0..k {
        let op = TransactionOutpoint::new(Hash::from_bytes([0xf1; 32]), 10_000 + j as u32);
        e.tx.inputs.insert(0, TransactionInput::new_with_compute_budget(op, vec![], 0, 0));
        e.entries.insert(0, UtxoEntry::new(1_000, p2pk_spk(&pk(MATCHER)), 500, false, None));
        e.plans.insert(0, SigPlan::P2pk { pubkey: pk(MATCHER) });
        e.tx.outputs.insert(
            0,
            kaspa_consensus_core::tx::TransactionOutput { value: 0, script_public_key: p2pk_spk(&pk(MATCHER)), covenant: None },
        );
    }
    let _ = std::marker::PhantomData::<TransactionOutput>;
    for o in e.tx.outputs.iter_mut() {
        if let Some(b) = o.covenant.as_mut() {
            b.authorizing_input += k as u16;
        }
    }
    e.tok_out = std::mem::take(&mut e.tok_out).into_iter().map(|(x, v)| (x + k, v)).collect();
    for p in e.plans.iter_mut() {
        if let SigPlan::Entry { template: TemplateId::KobPair, args, .. } = p {
            for a in [1usize, 2] {
                if let Arg::Int(v) = &mut args[a] {
                    *v += k as i64;
                }
            }
        }
    }
}

fn limits(r: &Run, pa: TemplateId, pb: TemplateId) {
    // ---- a KRON token UTXO holds at most 10^9 base units
    if pa.family() == Family::Kron {
        // PP60r an ask selling a KRON custody of 10^9 base units at 1 B per whole A (sold out)
        let s = PairState { price: 1, min_fill: KRON_MAX, amount_left: KRON_MAX, custody: KRON_MAX, ..ask0(pa, pb) };
        r.ok(&edb("PP60r ask of a 10^9-unit KRON custody, sold out", inv(s, pa, pb, KRON_MAX)));
        // PP61r a bid buying 10^9 base units of a KRON A (the matcher's KRON UTXO of 10^9)
        let mut s = PairState { price: 1, min_fill: KRON_MAX, amount_left: KRON_MAX, ..bid0(pa, pb) };
        s.custody = s.s_out(KRON_MAX, 1).unwrap();
        let mut b = route(vec![pair_leg_amount(s.clone(), pa, pb, OID, 70, KRON_MAX)]);
        b.taker_tokens = vec![tutxo(pa, TOKEN_COV, 60, KRON_MAX, pk(MATCHER), false)];
        r.ok(&edb("PP61r bid receiving 10^9 base units of a KRON A", b.clone()));
        // NP120r the same bid for 10^9 + 1 (one more unit from a second matcher UTXO): KobPair accepts it, the KRON
        // program refuses the delivery (fail closed)
        let mut e = edb("NP120r a KRON delivery of 10^9 + 1 base units", b);
        let i = oin(&e);
        restate(&mut e, i, |x| x.amount_left = KRON_MAX + 1);
        e.set_arg(i, 0, nb(KRON_MAX + 1));
        e.set_arg(i, 5, Arg::Int(KRON_MAX + 1));
        add_amt(&mut e, i, 1);
        e.add_token_input(utxo(61, CARRIER, 1_000, Some(TOKEN_COV)), user(pa, 1, MATCHER), Witness::P2pk(pk(MATCHER)));
        let first = e.first_token_input(TOKEN_COV);
        r.bad(&e, first);
    }
    // ---- a KRON custody is authorised by a one-byte witness (the order's input index): index 127 works, 128 fails
    if pa.family() == Family::Kron {
        let s = left_units(side_pr(true, pa, pb), NR);
        for (k, ok) in [(127usize, true), (128, false)] {
            let mut e = edb(
                &format!("{} ask of a KRON custody at input {k}", if ok { "PP62r" } else { "NP121r" }),
                inv(s.clone(), pa, pb, NR),
            );
            let i = oin(&e);
            shift_front(&mut e, k - i);
            assert_eq!(oin(&e), k);
            if ok {
                r.ok(&e);
            } else {
                let c = cin(&e, k);
                r.bad(&e, c);
            }
        }
    }
    // ---- overflow: every product is checked, a quote never wraps
    let (mut e, i) = inv_ed("NP122 the quote product overflows (price near 2^62, 4 whole A)", side_pr(true, pa, pb), pa, pb, NR);
    restate(&mut e, i, |x| x.price = i64::MAX / 3);
    r.bad(&e, i);
    let (mut e, i) = inv_ed("NP123 the tip product overflows", side_pr(true, pa, pb), pa, pb, NR);
    restate(&mut e, i, |x| x.tip = i64::MAX / 3);
    r.bad(&e, i);
    let (mut e, i) = inv_ed("NP124 the bid's quote product overflows", side_pr(false, pa, pb), pa, pb, NR);
    restate(&mut e, i, |x| x.price = i64::MAX / 3);
    r.bad(&e, i);
    let dutch = PairState {
        price: PR + 50,
        slope: 1,
        decay_step: 10,
        price_end: PR - 50,
        active_from: NOW as i64 - 600,
        ..side_pr(true, pa, pb)
    };
    let (mut e, i) = inv_ed("NP125 the decay product overflows (slope * steps)", dutch, pa, pb, NR);
    restate(&mut e, i, |x| x.slope = i64::MAX / 3);
    r.bad(&e, i);
}

#[test]
fn pair_limits() {
    suite(&mixed(), limits);
}

/// [`set_cont`] on the continuation of the order `OID`.
pub fn set_cont_o(e: &mut Ed, f: impl Fn(&mut PairState)) {
    let k = cont_of(e, OID).expect("continuation");
    set_cont(e, k, f);
}

/// `id` ("NPnn text") with the side letter appended to its first token.
pub fn sided(id: &str, side: &str) -> String {
    match id.split_once(' ') {
        Some((h, t)) => format!("{h}{side} {t}"),
        None => format!("{id}{side}"),
    }
}

/// A sold-out fill of NR at `PR` with nothing left at the custody's index (an ask: custody NR; a bid: its exact quote).
pub fn closing(ask: bool, pa: TemplateId, pb: TemplateId) -> PairState {
    let s = left_units(side_pr(ask, pa, pb), NR);
    if ask {
        s
    } else {
        PairState { custody: s.s_out(NR, PR).unwrap(), ..s }
    }
}
