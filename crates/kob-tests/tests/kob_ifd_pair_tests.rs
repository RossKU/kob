//! KobIfdPair (contracts/v2/KobIfdPair.sil) attack suite, and the repeat / merge checks of its exit KobCondPair.
//! Run: cargo test -p kob-tests --test kob_ifd_pair_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use fx::pair::*;
use fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{Action, BatchUpdate, Leg};
use kob_protocol::state::*;
use kob_protocol::tx::{Arg, SigPlan};
use ph::{built, family_mixes, Ed, Run, Subs};

fn hex4(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

fn dump(ed: &Ed) {
    println!("=== {}", ed.name);
    for (i, p) in ed.plans.iter().enumerate() {
        let e = &ed.entries[i];
        let cov = e.covenant_id.map(|h| hex4(&h.as_bytes())).unwrap_or("-".into());
        let what = match p {
            SigPlan::Entry { template, entry, args, .. } => {
                let a: Vec<String> = args
                    .iter()
                    .map(|a| match a {
                        Arg::Int(v) => v.to_string(),
                        Arg::Bytes(b) if b.len() == 8 => format!("nb{}", i64::from_le_bytes(b.as_slice().try_into().unwrap())),
                        Arg::Bytes(b) => format!("bytes{}", b.len()),
                        Arg::Sig(_) => "sig".into(),
                    })
                    .collect();
                format!("{}.{entry}({})", template.name(), a.join(","))
            }
            SigPlan::TokenLeader { state, .. } => format!("leader amt={} own={}", state.amount, hex4(&state.owner)),
            SigPlan::TokenDelegator { state, .. } => format!("deleg amt={} own={}", state.amount, hex4(&state.owner)),
            SigPlan::KronToken { state, .. } => format!("kron amt={} own={} t{}", state.amount, hex4(&state.owner), state.id_type),
            SigPlan::P2pk { pubkey } => format!("p2pk {}", hex4(pubkey)),
            _ => "other".into(),
        };
        println!("  in[{i}] cov={cov} v={} daa={} {what}", e.amount, e.block_daa_score);
    }
    for (k, o) in ed.tx.outputs.iter().enumerate() {
        let b = o.covenant.map(|c| format!("{}@{}", hex4(&c.covenant_id.as_bytes()), c.authorizing_input)).unwrap_or("-".into());
        let what = if let Some((c, st)) = ed.tok_out.get(&k) {
            format!("tok {} amt={} own={} cov={}", hex4(c), st.amount(), hex4(&st.owner()), st.is_covenant_owned())
        } else if let Some((id, _)) = ed.out_order(k) {
            format!("order {}", id.name())
        } else {
            "plain".into()
        };
        println!("  out[{k}] v={} bind={b} {what}", o.value);
    }
}

#[test]
#[ignore]
fn ifd_pair_dump() {
    let (pa, pb) = family_mixes()[1];
    for (name, a) in pair_scenarios(pa, pb) {
        if name.contains("ifd") || name.contains("rearm") {
            dump(&Ed::new(&name, &built(a)));
        }
    }
}

// ---------------------------------------------------------------- fixtures and editing helpers

use kob_protocol::build::{CancelOrder, RefundOrder};
use kob_protocol::tx::{TokenUtxo, Witness};
use TemplateId::{Kcc20Ref8x8, KronToken2433};

const IFD: TemplateId = TemplateId::KobIfdPair;
const COND: TemplateId = TemplateId::KobCondPair;

// KobIfdPair.fill arguments
const NB: usize = 0;
const AIN: usize = 1;
const BIN: usize = 2;
const ATPL: usize = 3;
const BTPL: usize = 4;
const AMT: usize = 6;
const EVA: usize = 9;
const EVB: usize = 10;
const TK: usize = 11;
const EVM: usize = 12;
const TT: usize = 13;
const XC: usize = 16;
const UPD: usize = 17;

/// The four family mixes (positives).
fn all_mixes() -> Vec<(TemplateId, TemplateId)> {
    family_mixes().to_vec()
}
/// The two mixed pairs (generic attacks: each family as A and as B).
fn mixed() -> Vec<(TemplateId, TemplateId)> {
    vec![(Kcc20Ref8x8, KronToken2433), (KronToken2433, Kcc20Ref8x8)]
}
/// The mixed pair whose A is KCC-20 (B KRON).
fn a_kcc() -> Vec<(TemplateId, TemplateId)> {
    vec![(Kcc20Ref8x8, KronToken2433)]
}
/// The mixed pair whose A is KRON (B KCC-20).
fn a_kron() -> Vec<(TemplateId, TemplateId)> {
    vec![(KronToken2433, Kcc20Ref8x8)]
}

/// Runs `f` on each program pair with a `Run` of the templates under test.
fn each(subs: &Subs, mixes: &[(TemplateId, TemplateId)], mut f: impl FnMut(&Run, TemplateId, TemplateId)) {
    for (pa, pb) in mixes {
        let r = Run { subs, pair: pair_name(*pa, *pb) };
        f(&r, *pa, *pb);
    }
}

/// A named shape of the fixtures (`pair_scenarios` or `pair_branch_shapes`).
fn shape(pa: TemplateId, pb: TemplateId, name: &str) -> Action {
    pair_scenarios(pa, pb)
        .into_iter()
        .chain(pair_branch_shapes(pa, pb))
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("no fixture shape {name}"))
        .1
}

fn ed_of(name: &str, a: Action) -> Ed {
    Ed::new(name, &built(a))
}

fn ed_shape(pa: TemplateId, pb: TemplateId, shape_name: &str, name: &str) -> Ed {
    ed_of(name, shape(pa, pb, shape_name))
}

/// The input spending the entry (covenant `E_ID`).
fn ein(ed: &Ed) -> usize {
    ed.input_of_cov(E_ID)
}
fn ist(ed: &Ed, i: usize) -> IfdPairState {
    IfdPairState::decode(&ed.entry_state(i).1).expect("entry state")
}
fn set_ist(ed: &mut Ed, i: usize, s: &IfdPairState) {
    ed.set_entry_state(i, s.encode());
}
fn arg(ed: &Ed, i: usize, pos: usize) -> i64 {
    match &ed.args(i)[pos] {
        Arg::Int(v) => *v,
        Arg::Bytes(b) if b.len() == 8 => i64::from_le_bytes(b.as_slice().try_into().unwrap()),
        a => panic!("arg {pos}: {a:?}"),
    }
}
fn set_int(ed: &mut Ed, i: usize, pos: usize, v: i64) {
    ed.set_arg(i, pos, Arg::Int(v));
}
fn set_nb(ed: &mut Ed, i: usize, v: i64) {
    ed.set_arg(i, NB, ph::nb(v));
}
/// The entry's continuation output (the only non-token output bound to `E_ID`).
fn cont(ed: &Ed) -> usize {
    let ks: Vec<usize> = ed.bound_to(E_ID).into_iter().filter(|k| !ed.is_tok_out(*k)).collect();
    assert_eq!(ks.len(), 1, "{}: entry continuation", ed.name);
    ks[0]
}
fn ost<S: StateCodec>(ed: &Ed, k: usize) -> S {
    S::decode(&ed.out_order(k).unwrap_or_else(|| panic!("{}: output {k} is not an order", ed.name)).1).expect("order state")
}
fn set_cont(ed: &mut Ed, k: usize, s: &IfdPairState) {
    ed.set_out_spk_state(k, IFD, &s.encode());
}
fn set_exit(ed: &mut Ed, k: usize, x: &CondPairState) {
    ed.set_out_spk_state(k, COND, &x.encode());
}
/// Token state of token input i.
fn tin(ed: &Ed, i: usize) -> TokenState {
    match &ed.plans[i] {
        SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. } => TokenState::Kcc20(state.clone()),
        SigPlan::KronToken { state, .. } => TokenState::Kron(state.clone()),
        p => panic!("{}: input {i} is not a token input: {p:?}", ed.name),
    }
}
fn set_tin(ed: &mut Ed, i: usize, st: TokenState) {
    match (&mut ed.plans[i], st) {
        (SigPlan::TokenLeader { state, .. } | SigPlan::TokenDelegator { state, .. }, TokenState::Kcc20(k)) => *state = k,
        (SigPlan::KronToken { state, .. }, TokenState::Kron(k)) => *state = k,
        (p, _) => panic!("input {i}: {p:?}"),
    }
}
fn prog(ed: &Ed, tok: [u8; 32]) -> TemplateId {
    ed.programs[&tok]
}
/// A key-held token state of `tok` for `key`.
fn user_st(ed: &Ed, tok: [u8; 32], amount: i64, key: u8) -> TokenState {
    let p = prog(ed, tok);
    TokenState::user(p.family(), amount, pk(key), ext_for(p))
}
/// A token state of `tok` owned by covenant `c`.
fn cov_st(ed: &Ed, tok: [u8; 32], amount: i64, c: [u8; 32]) -> TokenState {
    let p = prog(ed, tok);
    TokenState::custody(p.family(), amount, c, ext_for(p))
}
/// Pays `amount` of `tok` to the matcher (a new key-held token output).
fn give(ed: &mut Ed, tok: [u8; 32], amount: i64) -> usize {
    let st = user_st(ed, tok, amount, MATCHER);
    ed.add_token_output(tok, st, CARRIER)
}
/// Adds a token input of `tok` held by the matcher's key.
fn take(ed: &mut Ed, tok: [u8; 32], tag: u8, amount: i64) -> usize {
    let st = user_st(ed, tok, amount, MATCHER);
    ed.add_token_input(utxo(tag, CARRIER, 1_000, Some(tok)), st, Witness::P2pk(pk(MATCHER)))
}
/// Adds a token input of `tok` owned by covenant `c` (a stray / a custody look-alike).
fn stray(ed: &mut Ed, tok: [u8; 32], tag: u8, amount: i64, c: [u8; 32]) -> usize {
    let st = cov_st(ed, tok, amount, c);
    ed.add_token_input(utxo(tag, CARRIER, 1_000, Some(tok)), st, Witness::CovenantId)
}
/// Sets the amount of token output k.
fn set_out_amount(ed: &mut Ed, k: usize, amount: i64) {
    let st = ed.out_state(k).with_amount(amount);
    ed.set_out_state(k, st);
}
fn out_amount(ed: &Ed, k: usize) -> i64 {
    ed.out_state(k).amount()
}
fn set_tin_amount(ed: &mut Ed, i: usize, amount: i64) {
    let st = tin(ed, i).with_amount(amount);
    set_tin(ed, i, st);
}
/// The one exit order output (a KobCondPair UTXO).
fn exit_out(ed: &Ed) -> usize {
    let ks: Vec<usize> = (0..ed.tx.outputs.len()).filter(|k| matches!(ed.out_order(*k), Some((COND, _)))).collect();
    assert_eq!(ks.len(), 1, "{}: exit output", ed.name);
    ks[0]
}
fn exit_state(ed: &Ed) -> CondPairState {
    ost::<CondPairState>(ed, exit_out(ed))
}
/// The covenant id bound at output k.
fn cov_at(ed: &Ed, k: usize) -> [u8; 32] {
    ed.tx.outputs[k].covenant.unwrap_or_else(|| panic!("{}: output {k} has no binding", ed.name)).covenant_id.as_bytes()
}
/// The exit's custody token output (owned by the exit's genesis id).
fn exit_cust(ed: &Ed) -> usize {
    let xid = cov_at(ed, exit_out(ed));
    let ks: Vec<usize> =
        ed.tok_out.keys().copied().filter(|k| ed.out_state(*k).is_covenant_owned() && ed.out_state(*k).owner() == xid).collect();
    assert_eq!(ks.len(), 1, "{}: exit custody", ed.name);
    ks[0]
}
/// A token output owned by the entry id (`E_ID`) of token `tok` (the escrow rest / A or B custody rest).
fn entry_rest(ed: &Ed, tok: [u8; 32]) -> Option<usize> {
    ed.tok_outs_of(tok).into_iter().find(|k| ed.out_state(*k).is_covenant_owned() && ed.out_state(*k).owner() == E_ID)
}
/// A token input of `tok` owned by covenant `c` (a custody input).
fn cust_in(ed: &Ed, tok: [u8; 32], c: [u8; 32]) -> usize {
    (0..ed.plans.len())
        .find(|i| {
            ed.entries[*i].covenant_id.map(|h| h.as_bytes()) == Some(tok)
                && !matches!(ed.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. })
                && tin(ed, *i).is_covenant_owned()
                && tin(ed, *i).owner() == c
        })
        .unwrap_or_else(|| panic!("{}: no custody input of token owned by {}", ed.name, hex4(&c)))
}

/// The cancel of an entry by its maker (custodies and strays of both tokens swept).
fn cancel_entry(s: IfdPairState, pa: TemplateId, pb: TemplateId, strays: Vec<TokenUtxo>) -> Action {
    let a_cust = (!s.is_buy_first() && s.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, 71, s.amount_left, E_ID, true));
    let b_cust = (s.custody > 0).then(|| tutxo(pb, TOKEN_B, 72, s.custody, E_ID, true));
    let (custody, prefund) = if a_cust.is_some() { (a_cust, b_cust) } else { (b_cust, None) };
    let v = ifd_value(&s);
    Action::CancelOrder(CancelOrder {
        order: order(70, v, E_ID, 1_000, AnyState::KobIfdPair(s)),
        custody,
        prefund,
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

/// The refund of an entry (anyone, after expiry).
fn refund_entry(s: IfdPairState, pa: TemplateId, pb: TemplateId) -> Action {
    let a_cust = (!s.is_buy_first() && s.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, 71, s.amount_left, E_ID, true));
    let b_cust = (s.custody > 0).then(|| tutxo(pb, TOKEN_B, 72, s.custody, E_ID, true));
    let (custody, prefund) = if a_cust.is_some() { (a_cust, b_cust) } else { (b_cust, None) };
    let v = ifd_value(&s);
    Action::RefundOrder(RefundOrder {
        order: order(70, v, E_ID, 1_000, AnyState::KobIfdPair(s)),
        foreign: vec![],
        custody,
        prefund,
        lock_time: EXPIRY as u64,
        funding: vec![key_utxo(250, KEEPER, 5 * KAS)],
        change: Some(pk(KEEPER)),
        fee: fee(),
    })
}

/// A stop entry (trigger RATE, limit RATE +- 50, `armed`) of side `buy`.
fn stop_entry(pa: TemplateId, pb: TemplateId, buy: bool, armed: i64) -> IfdPairState {
    let mut s = ifd_pair(MAKER_A, buy, pa, pb);
    s.entry_stop = RATE;
    s.price = if buy { RATE + 50 } else { RATE - 50 };
    s.armed = armed;
    s.custody = s.b_custody_needed().unwrap();
    s
}

/// Evidence legs, token-balanced (every evidence fill settled through the opposite KAS book / pair order): mode 0 a
/// KAS-book order of A and one of B (legs 0, 1), mode 1 a resting KobPair (leg 0). `fell`: sell-trigger evidence.
fn ev_legs(pa: TemplateId, pb: TemplateId, mode: u8, fell: bool) -> Vec<Leg> {
    match (mode, fell) {
        (0, true) => vec![
            kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 120, WHOLE),
            kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 122, WHOLE),
            kbid_units(TOKEN_COV, pa, 24, P260, cov(0x95), 124, WHOLE),
            kask_units(TOKEN_B, pb, 25, P250, cov(0x96), 126, WHOLE),
        ],
        (0, false) => vec![
            kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 120, WHOLE),
            kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 122, WHOLE),
            kask_units(TOKEN_COV, pa, 24, P250, cov(0x95), 124, WHOLE),
            kbid_units(TOKEN_B, pb, 25, P260, cov(0x96), 126, WHOLE),
        ],
        (1, true) => vec![
            pair_leg(pair(MAKER_C, true, pa, pb, RATE - 10), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
        _ => vec![
            pair_leg(pair(MAKER_C, false, pa, pb, RATE + 10), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
    }
}

/// An update arming the stop entry `s` next to the evidence of `mode` (the keeper takes keeperTip).
fn update_entry(s: IfdPairState, pa: TemplateId, pb: TemplateId, mode: u8) -> Action {
    let fell = !s.is_buy_first();
    let mut b = route(ev_legs(pa, pb, mode, fell));
    b.updates.push(BatchUpdate {
        order: order(60, ifd_value(&s), E_ID, 1_000, AnyState::KobIfdPair(s)),
        evidence: 0,
        evidence_b: (mode == 0).then_some(1),
        take: None,
    });
    Action::Batch(b)
}

// ---------------------------------------------------------------- positives

/// Every honest entry shape (the fixtures' entry and re-arm shapes plus this suite's own), named `PI<nn> <shape>`.
fn positives(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    for (name, a) in pair_scenarios(pa, pb).into_iter().chain(pair_branch_shapes(pa, pb)) {
        let ifd = name.contains("ifd") || name.contains("rearm") || name.contains("Ifd");
        if ifd && !name.contains("create") {
            v.push((name, a));
        }
    }
    for buy in [true, false] {
        let side = if buy { "bid" } else { "ask" };
        for mode in [0u8, 1] {
            v.push((format!("suite.update.{side}.ev{mode}"), update_entry(stop_entry(pa, pb, buy, 0), pa, pb, mode)));
        }
        let s = ifd_pair(MAKER_A, buy, pa, pb);
        let strays = vec![tutxo(pa, TOKEN_COV, 76, 3, E_ID, true), tutxo(pb, TOKEN_B, 77, 5, E_ID, true)];
        v.push((format!("suite.cancel.{side}.strays"), cancel_entry(s.clone(), pa, pb, strays)));
        v.push((format!("suite.refund.{side}"), refund_entry(s.clone(), pa, pb)));
    }
    // a repeating sell-first entry with nothing left (waiting for its exits): the refund returns the prefund only
    let s = ifd_pair(MAKER_A, false, pa, pb);
    let w = IfdPairState { amount_left: 0, rpt_amount: 1 + 4 * WHOLE, custody: 5, ..s };
    v.push(("suite.refund.ask.waiting".into(), refund_entry(w, pa, pb)));
    v.into_iter().enumerate().map(|(k, (n, a))| (format!("PI{:02} {n}", k + 1), a)).collect()
}

#[test]
fn ifd_pair_positives() {
    let subs = Subs::compile();
    let mut count = 0;
    each(&subs, &all_mixes(), |r, pa, pb| {
        for (name, a) in positives(pa, pb) {
            r.ok(&ed_of(&name, a));
            count += 1;
        }
    });
    println!("ifd pair positives: {count}");
}

// ---------------------------------------------------------------- exit / merge input helpers

fn xin(ed: &Ed) -> usize {
    ed.input_of_cov(XE_ID)
}
fn xst(ed: &Ed, i: usize) -> CondPairState {
    CondPairState::decode(&ed.entry_state(i).1).expect("exit state")
}
/// A token output owned by the matcher key of `tok` (a taker output), if any.
fn matcher_out(ed: &Ed, tok: [u8; 32]) -> Option<usize> {
    ed.tok_outs_of(tok).into_iter().find(|k| !ed.out_state(*k).is_covenant_owned() && ed.out_state(*k).owner() == pk(MATCHER))
}

// ---------------------------------------------------------------- fill: buy-first (BID)

#[test]
fn ifd_fill_buy() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let base = |name: &str| ed_shape(pa, pb, "pair.ifd.bid.cont", name);
        // NI01 the B released is floor + 1 (the escrow overpays by one base unit)
        {
            let mut e = base("NI01 buy sOut floor + 1");
            let i = ein(&e);
            let s = ist(&e, i);
            let (p, n) = (s.price, arg(&e, i, NB));
            let over = s.spend(n, p).unwrap() + 1;
            let bnew = s.custody - over;
            set_int(&mut e, i, AMT, over);
            let rest = entry_rest(&e, TOKEN_B).expect("escrow rest");
            set_out_amount(&mut e, rest, bnew);
            let c = cont(&e);
            let cs = IfdPairState { custody: bnew, ..ost::<IfdPairState>(&e, c) };
            set_cont(&mut e, c, &cs);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, i);
        }
        // NI03 the delivery (output i: the exit custody) is short one sompi of the delivery carrier
        {
            let mut e = base("NI03 buy delivery carrier short");
            let i = ein(&e);
            let k = exit_cust(&e);
            e.set_value(k, e.value(k) - 1);
            r.bad(&e, i);
        }
        // NI04 the continuation is short one sompi
        {
            let mut e = base("NI04 buy continuation floor short");
            let i = ein(&e);
            let c = cont(&e);
            e.set_value(c, e.value(c) - 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- fill: sell-first (ASK)

#[test]
fn ifd_fill_sell() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let base = |name: &str| ed_shape(pa, pb, "pair.ifd.ask.cont", name);
        // NI05 the proceeds put into the exit custody are ceil - 1 (the maker is underpaid by one base unit)
        {
            let mut e = base("NI05 sell tOut ceil - 1");
            let i = ein(&e);
            let s = ist(&e, i);
            let (p, n) = (s.price, arg(&e, i, NB));
            let proceeds = s.proceeds(n, p).unwrap();
            let used = s.pre_of(n).unwrap();
            set_int(&mut e, i, AMT, proceeds - 1);
            let k = exit_cust(&e);
            set_out_amount(&mut e, k, proceeds - 1 + used);
            let x = CondPairState { custody: proceeds - 1 + used, ..exit_state(&e) };
            let xo = exit_out(&e);
            set_exit(&mut e, xo, &x);
            e.rebind_genesis(i as u16);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, i);
        }
        // NI06 the delivery (output i: the exit custody) is short one sompi
        {
            let mut e = base("NI06 sell delivery carrier short");
            let i = ein(&e);
            let k = exit_cust(&e);
            e.set_value(k, e.value(k) - 1);
            r.bad(&e, i);
        }
        // NI07 the continuation is short one sompi
        {
            let mut e = base("NI07 sell continuation floor short");
            let i = ein(&e);
            let c = cont(&e);
            e.set_value(c, e.value(c) - 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- exact custodies

#[test]
fn ifd_custody() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI09 buy-first: the B escrow custody holds one base unit more than the state (the extra is skimmed)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI09 buy B escrow custody one over");
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_B, E_ID);
            let camt = tin(&e, ci).amount() + 1;
            set_tin_amount(&mut e, ci, camt);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, i);
        }
        // NI10 sell-first: the A custody holds one base unit more than amountLeft
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NI10 sell A custody one over");
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_COV, E_ID);
            let camt = tin(&e, ci).amount() + 1;
            set_tin_amount(&mut e, ci, camt);
            give(&mut e, TOKEN_COV, 1);
            r.bad(&e, i);
        }
        // NI11 sell-first: the B prefund custody holds one base unit more than the state
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NI11 sell B prefund custody one over");
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_B, E_ID);
            let camt = tin(&e, ci).amount() + 1;
            set_tin_amount(&mut e, ci, camt);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- strays of both tokens, every path

#[test]
fn ifd_strays() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI12 a stray of A owned by the entry on a FILL (buy-first)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI12 stray A on fill");
            let i = ein(&e);
            stray(&mut e, TOKEN_COV, 160, 7, E_ID);
            give(&mut e, TOKEN_COV, 7);
            r.bad(&e, i);
        }
        // NI13 a stray of B owned by the entry on a FILL (sell-first)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NI13 stray B on fill");
            let i = ein(&e);
            stray(&mut e, TOKEN_B, 160, 7, E_ID);
            give(&mut e, TOKEN_B, 7);
            r.bad(&e, i);
        }
        // NI14 a stray of B owned by the entry on a REFUND (sell-first)
        {
            let mut e = ed_of("NI14 stray B on refund", refund_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb));
            let i = ein(&e);
            stray(&mut e, TOKEN_B, 160, 7, E_ID);
            give(&mut e, TOKEN_B, 7);
            r.bad(&e, i);
        }
        // NI15 a stray of A owned by the entry on an UPDATE (spends no custody at all)
        {
            let mut e = ed_of("NI15 stray A on update", update_entry(stop_entry(pa, pb, true, 0), pa, pb, 1));
            let i = ein(&e);
            stray(&mut e, TOKEN_COV, 160, 7, E_ID);
            give(&mut e, TOKEN_COV, 7);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- update / refund / fill encodings

#[test]
fn ifd_encoding() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI25 update (nb == 0, upd 1) that spends a custody (a token input owned by the entry)
        {
            let mut e = ed_of("NI25 update with a custody input", update_entry(stop_entry(pa, pb, true, 0), pa, pb, 1));
            let i = ein(&e);
            let st = cov_st(&e, TOKEN_B, 5, E_ID);
            e.add_token_input(utxo(162, CARRIER, 1_000, Some(TOKEN_B)), st, Witness::CovenantId);
            give(&mut e, TOKEN_B, 5);
            r.bad(&e, i);
        }
        // NI26 upd outside 0 / 1 (upd = 2 with nb = 0)
        {
            let mut e = ed_of("NI26 upd = 2", update_entry(stop_entry(pa, pb, true, 0), pa, pb, 1));
            let i = ein(&e);
            set_int(&mut e, i, UPD, 2);
            r.bad(&e, i);
        }
        // NI27 upd = 1 with a positive nb
        {
            let mut e = ed_of("NI27 upd 1 with nb > 0", update_entry(stop_entry(pa, pb, true, 0), pa, pb, 1));
            let i = ein(&e);
            set_nb(&mut e, i, 4000);
            r.bad(&e, i);
        }
        // NI28 a sibling UTXO of the entry covenant id (OpCovInputCount(selfId) == 1)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI28 entry has a sibling of its id");
            let i = ein(&e);
            let e2 = e.entries[i].clone();
            e.tx.inputs.push(e.tx.inputs[i].clone());
            e.entries.push(e2);
            e.plans.push(e.plans[i].clone());
            e.add_plain_output(KAS, MATCHER);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- refund

#[test]
fn ifd_refund() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI33 refund before the idle / expiry time
        {
            let mut e = ed_of("NI33 refund before idleEnd", refund_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb));
            let i = ein(&e);
            e.set_input_daa(i, 1_000);
            e.tx.lock_time = 1_500;
            r.bad(&e, i);
        }
        // NI34 the maker's KAS at output i is short of the UTXO value minus refundTip
        {
            let mut e = ed_of("NI34 refund maker KAS short", refund_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb));
            let i = ein(&e);
            e.set_value(i, e.value(i) - 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- KobCondPair settle args (the exit)

const C_NB: usize = 0;
const C_CUST: usize = 1;
const C_T: usize = 3;
const C_TOUT: usize = 5;
const C_LEG: usize = 6;

fn set_bind(ed: &mut Ed, k: usize, auth: usize, cov: [u8; 32]) {
    ed.tx.outputs[k].covenant = Some(kaspa_consensus_core::tx::CovenantBinding {
        authorizing_input: auth as u16,
        covenant_id: kaspa_consensus_core::Hash::from_bytes(cov),
    });
}

// ---------------------------------------------------------------- the exit: genesis forgery and pins

#[test]
fn ifd_exit_genesis() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI17 the exit custody holds n - 1 of A (one A fewer delivered than the entry bought; buy-first)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI17 buy exit custody short");
            let i = ein(&e);
            let k = exit_cust(&e);
            let short = out_amount(&e, k) - 1;
            set_out_amount(&mut e, k, short);
            give(&mut e, TOKEN_COV, 1);
            r.bad(&e, i);
        }
        // NI18 the exit state commits amountLeft = n + 1 (not the fill n)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI18 exit amountLeft != n");
            let i = ein(&e);
            let x = CondPairState { amount_left: exit_state(&e).amount_left + 1, ..exit_state(&e) };
            let xo = exit_out(&e);
            set_exit(&mut e, xo, &x);
            e.rebind_genesis(i as u16);
            r.bad(&e, i);
        }
        // NI19 the exit is a TWO-output genesis group (the covenant checks a single-output group: the recomputed id of a
        // size-2 group differs from the contract's size-1 derivation, so the exit output's real id is not the one the
        // covenant requires). A valid transaction; the exit genesis-id check is the only rejection.
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI19 exit genesis group has a second output");
            let i = ein(&e);
            let xid = cov_at(&e, exit_out(&e));
            let extra = e.add_plain_output(EC as u64, MATCHER);
            set_bind(&mut e, extra, i, xid);
            e.rebind_genesis(i as u16);
            r.bad(&e, i);
        }
        // NI20 the exit's committed prefix does not hash to COND_TPL (one byte flipped)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI20 exit prefix hash wrong");
            let i = ein(&e);
            let mut a = e.args(i);
            if let Arg::Bytes(b) = &mut a[7] {
                b[0] ^= 0x01;
            }
            e.set_entry(i, "fill", a);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- aliasing

#[test]
fn ifd_aliasing() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI23 the exit's custody is placed at the wrong output index (not the entry's own input index)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NI23 exit custody at the wrong index");
            let i = ein(&e);
            let k = exit_cust(&e);
            // move the exit custody off the self index by swapping with a trailing plain output
            let other = e.add_plain_output(e.value(k), MATCHER);
            e.swap_outputs(k, other);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- update checks

#[test]
fn ifd_update() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NI30 the keeper takes more than keeperTip from the entry UTXO (the continuation is one sompi short of the floor)
        {
            let mut e = ed_of("NI30 update keeper over keeperTip", update_entry(stop_entry(pa, pb, true, 0), pa, pb, 1));
            let i = ein(&e);
            let c = cont(&e);
            e.set_value(c, e.value(c) - 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- merge: the entry's custody growth (entry side)

#[test]
fn ifd_merge() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NIM1 buy-first merge into an empty entry: the NEW B custody is short of exitCarrier
        {
            let mut e = ed_shape(pa, pb, "pair.rearm.bid.new", "NIM1 buy merge new B custody carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_B).expect("new B custody");
            e.set_value(k, e.value(k) - 1);
            r.bad(&e, i);
        }
        // NIM2 sell-first merge into an empty entry: the NEW A custody is short of exitCarrier
        {
            let mut e = ed_shape(pa, pb, "pair.rearm.ask.new", "NIM2 sell merge new A custody carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_COV).expect("new A custody");
            e.set_value(k, e.value(k) - 1);
            r.bad(&e, i);
        }
        // NIM3 buy-first merge sell-out: the escrow grows by one base unit LESS than budget(m) (the entry is skimmed)
        {
            let mut e = ed_shape(pa, pb, "pair.rearm.bid.sellout", "NIM3 buy merge escrow growth short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_B).expect("merged escrow");
            let short = out_amount(&e, k) - 1;
            set_out_amount(&mut e, k, short);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, i);
        }
        // NIM4 sell-first merge sell-out: the A custody grows by one base unit LESS than m
        {
            let mut e = ed_shape(pa, pb, "pair.rearm.ask.sellout", "NIM4 sell merge A growth short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_COV).expect("merged A custody");
            let short = out_amount(&e, k) - 1;
            set_out_amount(&mut e, k, short);
            give(&mut e, TOKEN_COV, 1);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- the re-arming exit (KobCondPair repeat checks)

#[test]
fn cond_rpt_profit() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NCR1 a buy-first re-arm: the re-arming exit (a KobCondPair ASK take-profit) pays the maker one base unit of B
        // less than its profit (the difference is skimmed). The exit input rejects.
        {
            let mut e = ed_shape(pa, pb, "pair.rearm.bid.sellout", "NCR1 re-arm exit maker profit short");
            let x = xin(&e);
            let k = e
                .tok_outs_of(TOKEN_B)
                .into_iter()
                .find(|k| !e.out_state(*k).is_covenant_owned() && e.out_state(*k).owner() == pk(MAKER_A))
                .expect("maker profit output");
            let short = out_amount(&e, k) - 1;
            set_out_amount(&mut e, k, short);
            give(&mut e, TOKEN_B, 1);
            r.bad(&e, x);
        }
    });
}

// ---------------------------------------------------------------- stop-entry trigger evidence (exposure)

#[test]
fn ifd_evidence() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NEV1 evMode 1 (a resting pair order): the evidence order was not exposed for minRestDaa before the fill (its
        // UTXO DAA is raised to the lock time). Read by rd().
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.stop.ev1", "NEV1 ev1 evidence unexposed");
            let i = ein(&e);
            let ev = e.input_of_cov(cov(0x93));
            e.set_input_daa(ev, NOW);
            r.bad(&e, i);
        }
        // NEV2 evMode 0, the B leg (a bid of B): unexposed. Read by evLeg().
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.stop.ev0", "NEV2 ev0 B-leg unexposed");
            let i = ein(&e);
            let ev = e.input_of_cov(cov(0x94));
            e.set_input_daa(ev, NOW);
            r.bad(&e, i);
        }
        // NEV3 evMode 0, the A leg (an ask of A): unexposed. Read by rd().
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.stop.ev0", "NEV3 ev0 A-leg unexposed");
            let i = ein(&e);
            let ev = e.input_of_cov(cov(0x93));
            e.set_input_daa(ev, NOW);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- the booked exit takes profit WITHOUT its entry

/// A booked exit (parent E_ID) taking profit on leg 0 through the KAS books with NO entry spent and NO merge: legal when
/// tx.daa >= rptUntil (`until` small). Buy-first entry -> exit ASK (sells A); sell-first -> exit BID (buys A back).
fn booked_tp_alone(pa: TemplateId, pb: TemplateId, buy: bool, until: i64) -> Action {
    let base = ifd_pair(MAKER_A, buy, pa, pb);
    let mut x = booked_exit(&base, 4);
    x.rpt_until = until;
    let leg0 = cond_pair_leg(x, pa, pb, XE_ID, 80, 4, 0);
    if buy {
        Action::Batch(route(vec![
            leg0,
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 5),
        ]))
    } else {
        Action::Batch(route(vec![
            leg0,
            kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 72, 10, 3),
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ]))
    }
}

#[test]
fn cond_rpt_until() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // positive twin: a booked exit may take profit without its entry once tx.daa >= rptUntil
        r.ok(&ed_of("PIR booked exit TP without entry (daa >= rptUntil)", booked_tp_alone(pa, pb, true, 2_000)));
        // NCR_until: the same, but rptUntil is in the future (> lock). The exit input must reject (a booked exit takes
        // profit early only together with its entry's merge).
        {
            let mut e = ed_of("NCRU booked exit TP without entry before rptUntil", booked_tp_alone(pa, pb, true, 2_000));
            let x = xin(&e);
            let s = CondPairState { rpt_until: NOW as i64 + 1, ..xst(&e, x) };
            e.set_entry_state(x, s.encode());
            r.bad(&e, x);
        }
    });
}

// ================================================================ completion (p2t_ifd2): every require of KobIfdPair

use kob_protocol::build::Batch;
use kob_protocol::tx::Witness as Wit;

/// KobIfdPair.quoteOf, exactly as the engine computes it (truncating division).
fn qof(n: i64, r: i64, d: i64, c: i64) -> i64 {
    let (q, m) = (n / d, n % d);
    q * r + m * (r / d) + (m * (r % d) + c) / d
}
/// The repeat merge argument `-(k * 2^53 + m)` as the 8-byte sign-magnitude script number the entry pushes.
fn merge_nb(k: i64, m: i64) -> Arg {
    let mut b = (k * (1i64 << 53) + m).to_le_bytes();
    b[7] |= 0x80;
    Arg::Bytes(b.to_vec())
}
/// The entry's continuation output, if any.
fn cont_opt(e: &Ed) -> Option<usize> {
    e.bound_to(E_ID).into_iter().find(|k| !e.is_tok_out(*k))
}
/// Applies `f` to the entry input's state and to the continuation's (an immutable field stays equal in both).
fn both(e: &mut Ed, i: usize, f: impl Fn(&mut IfdPairState)) {
    let mut s = ist(e, i);
    f(&mut s);
    set_ist(e, i, &s);
    if let Some(k) = cont_opt(e) {
        let mut c = ost::<IfdPairState>(e, k);
        f(&mut c);
        set_cont(e, k, &c);
    }
}
/// Applies `f` to the continuation's state only (a mutable field the attack changes).
fn cont_state(e: &mut Ed, f: impl Fn(&mut IfdPairState)) {
    let k = cont(e);
    let mut c = ost::<IfdPairState>(e, k);
    f(&mut c);
    set_cont(e, k, &c);
}
/// Edits the exit output's state and recomputes its genesis id (authorised by the entry input `i`).
fn edit_exit(e: &mut Ed, i: usize, f: impl Fn(&mut CondPairState)) {
    let xo = exit_out(e);
    let mut x = exit_state(e);
    f(&mut x);
    set_exit(e, xo, &x);
    e.rebind_genesis(i as u16);
}
/// Token input `i` becomes a key-held UTXO of `key` (the same amount; the key signs it).
fn rekey_tin(e: &mut Ed, i: usize, key: u8) {
    let st = tin(e, i).with_user_owner(pk(key));
    match (&mut e.plans[i], st) {
        (SigPlan::TokenLeader { state, witness, .. } | SigPlan::TokenDelegator { state, witness, .. }, TokenState::Kcc20(k)) => {
            *state = k;
            *witness = Wit::P2pk(pk(key));
        }
        (SigPlan::KronToken { state, .. }, TokenState::Kron(k)) => *state = k,
        (p, _) => panic!("input {i}: {p:?}"),
    }
}
/// Token output `k` becomes key-held by `key` (same amount).
fn rekey_out(e: &mut Ed, k: usize, key: u8) {
    let st = e.out_state(k).with_user_owner(pk(key));
    e.set_out_state(k, st);
}
/// The entry UTXO of `s` filled for `n` base units (custodies as a resting entry of that state).
fn ifd_leg_units(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64, t: Option<i64>) -> Leg {
    let mut l = ifd_pair_leg(s, pa, pb, E_ID, 70, 1);
    if let Leg::IfdPair { amount, t: lt, .. } = &mut l {
        *amount = n;
        *lt = t;
    }
    l
}
/// A buy-first fill of `n` base units through the KAS books at quote `p`: the B released (spend) sold to a bid of B, the
/// n of A bought from an ask of A.
fn buy_fill(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64, p: i64, t: Option<i64>) -> Batch {
    let amt = s.spend(n, p).unwrap();
    route(vec![
        ifd_leg_units(s, pa, pb, n, t),
        kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 74, amt),
        kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 76, n),
    ])
}
/// A sell-first fill of `n` base units at quote `p`: the n of A sold to a bid of A, the proceeds bought from an ask of B
/// (none when they are 0).
fn sell_fill(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64, p: i64, t: Option<i64>) -> Batch {
    let proceeds = qof(n, p, s.a_scale, s.a_scale - 1);
    let mut legs = vec![ifd_leg_units(s, pa, pb, n, t), kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 74, n)];
    if proceeds > 0 {
        legs.push(kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 76, proceeds));
    }
    route(legs)
}
fn edb(name: &str, b: Batch) -> Ed {
    Ed::new(name, &built(Action::Batch(b)))
}
fn edu(name: &str, b: Batch) -> Ed {
    Ed::new(name, &ph::unchecked(b))
}
/// The change output gets `d` sompi more (balances a value cut elsewhere).
fn to_change(e: &mut Ed, d: u64) {
    let c = e.change_out();
    let v = e.value(c);
    e.set_value(c, v + d);
}

// ---------------------------------------------------------------- fill rules

#[test]
fn ifd_fill_rules() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let ib = ifd_pair(MAKER_A, true, pa, pb);
        let ia = ifd_pair(MAKER_A, false, pa, pb);
        // PIF1 / NIF03 a fill of exactly minFill (not the rest) is accepted, one base unit less is refused
        r.ok(&edb("PIF1 buy fill of exactly minFill", buy_fill(ib.clone(), pa, pb, WHOLE, RATE, None)));
        {
            // built with minFill lowered by one, then the state's minFill restored
            let low = IfdPairState { min_fill: WHOLE - 1, ..ib.clone() };
            let mut e = edb("NIF03 buy fill of minFill - 1 (not the rest)", buy_fill(low, pa, pb, WHOLE - 1, RATE, None));
            let i = ein(&e);
            both(&mut e, i, |s| s.min_fill = WHOLE);
            r.bad(&e, i);
        }
        // NIF01 a fill before activeFrom
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIF01 fill before activeFrom");
            let i = ein(&e);
            both(&mut e, i, |s| s.active_from = NOW as i64 + 1);
            r.bad(&e, i);
        }
        // NIF02 a fill of amountLeft + 1 (the escrow pays the same B, the exit gets one more A from the matcher)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.close", "NIF02 fill of amountLeft + 1");
            let i = ein(&e);
            let n = arg(&e, i, NB) + 1;
            set_nb(&mut e, i, n);
            let k = exit_cust(&e);
            set_out_amount(&mut e, k, n);
            edit_exit(&mut e, i, |x| {
                x.amount_left = n;
                x.custody = n;
            });
            take(&mut e, TOKEN_COV, 161, 1);
            r.bad(&e, i);
        }
        // NIF04 / NIF05 / NIF06 hostile negative tip / deliveryCarrier / exitCarrier (the filler funds the continuation)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIF04 hostile negative tip");
            let i = ein(&e);
            both(&mut e, i, |s| s.tip = -WHOLE);
            let (c, n) = (cont(&e), arg(&e, i, NB));
            e.fund_out(c, n);
            r.bad(&e, i);
        }
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIF05 hostile negative deliveryCarrier");
            let i = ein(&e);
            both(&mut e, i, |s| s.delivery_carrier = -1);
            let c = cont(&e);
            e.fund_out(c, PDC + 1);
            r.bad(&e, i);
        }
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIF06 hostile negative exitCarrier");
            let i = ein(&e);
            both(&mut e, i, |s| s.exit_carrier = -1);
            let c = cont(&e);
            e.fund_out(c, PEC + 1);
            r.bad(&e, i);
        }
        // NIF07 / NIF08 the stop ordering at a fill: a buy stop above its limit / a sell stop below it (armed entries in
        // their auction; the quote stays within what the honest amounts satisfy)
        {
            let mut e = ed_shape(pa, pb, "pairgrid.ifd.bid.auction.cont", "NIF07 buy stop entry above its limit (fill)");
            let i = ein(&e);
            both(&mut e, i, |s| s.entry_stop = s.price + 1);
            r.bad(&e, i);
        }
        {
            let mut e = ed_shape(pa, pb, "pairgrid.ifd.ask.auction.cont", "NIF08 sell stop entry below its limit (fill)");
            let i = ein(&e);
            both(&mut e, i, |s| s.entry_stop = s.price - 1);
            r.bad(&e, i);
        }
        // NIF09 the auction time t after the lock time (not proven by CLTV)
        {
            let mut e = ed_shape(pa, pb, "pairgrid.ifd.bid.auction.cont", "NIF09 auction t after the lock time");
            let i = ein(&e);
            set_int(&mut e, i, TT, NOW as i64 + 1);
            r.bad(&e, i);
        }
        // PIF10 / NIF10 an entry armed at the lock time: t = origin accepted, t = origin - 1 refused (the quote is the same:
        // the band term truncates to 0)
        {
            let s = IfdPairState { entry_stop: RATE, price: RATE + 50, armed: NOW as i64, ..ib.clone() };
            let s = IfdPairState { custody: s.b_custody_needed().unwrap(), ..s };
            r.ok(&edb("PIF10 auction at its origin (t = armed)", buy_fill(s.clone(), pa, pb, 4 * WHOLE, RATE, Some(NOW as i64))));
            let mut e = edb("NIF10 auction t before its origin", buy_fill(s, pa, pb, 4 * WHOLE, RATE, Some(NOW as i64)));
            let i = ein(&e);
            set_int(&mut e, i, TT, NOW as i64 - 1);
            r.bad(&e, i);
        }
        // NIF11 a sell-first entry at price 0 (the A goes for nothing; the exit holds only the prefund)
        {
            // built at price 1 (the proceeds 4 base units still go into the exit), then the state's price set to 0
            let s = IfdPairState { price: 1, ..ia.clone() };
            let mut e = edb("NIF11 sell-first fill at price 0", sell_fill(s, pa, pb, 4 * WHOLE, 1, None));
            let i = ein(&e);
            both(&mut e, i, |s| s.price = 0);
            r.bad(&e, i);
        }
        // NIF13 a booking whose rptUntil is dated by the filler's time argument instead of the entry UTXO's DAA
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.book", "NIF13 booking dated by the filler's t");
            let i = ein(&e);
            set_int(&mut e, i, TT, 5_000);
            let until = EXPIRY.min(1_000 + 77_760_000) + 4_000;
            edit_exit(&mut e, i, |x| x.rpt_until = until);
            r.bad(&e, i);
        }
        // NIF14 buy-first: a negative B release (the escrow rest grows; the matcher funds it)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIF14 buy negative sOut");
            let i = ein(&e);
            let s = ist(&e, i);
            let n = arg(&e, i, NB);
            let released = arg(&e, i, AMT);
            set_int(&mut e, i, AMT, -1);
            let k = entry_rest(&e, TOKEN_B).expect("escrow rest");
            set_out_amount(&mut e, k, s.custody + 1);
            cont_state(&mut e, |c| c.custody = s.custody + 1);
            let _ = n;
            take(&mut e, TOKEN_B, 161, released + 1);
            r.bad(&e, i);
        }
        // NIF15 buy-first: the release exceeds the escrow (custody - 1 held, the full floor released, the matcher adds 1)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.close", "NIF15 buy release beyond the escrow");
            let i = ein(&e);
            let s = ist(&e, i);
            set_ist(&mut e, i, &IfdPairState { custody: s.custody - 1, ..s.clone() });
            let ci = cust_in(&e, TOKEN_B, E_ID);
            set_tin_amount(&mut e, ci, s.custody - 1);
            take(&mut e, TOKEN_B, 161, 1);
            r.bad(&e, i);
        }
        // NIF16 sell-first: a hostile negative prefund (the exit's custody shrinks, the prefund custody grows)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIF16 sell hostile negative prefund");
            let i = ein(&e);
            let s = ist(&e, i);
            let n = arg(&e, i, NB);
            let used = qof(n, -s.prefund, s.a_scale, s.a_scale - 1);
            let amt = arg(&e, i, AMT);
            both(&mut e, i, |x| x.prefund = -x.prefund);
            cont_state(&mut e, |c| c.custody = s.custody - used);
            let rest = entry_rest(&e, TOKEN_B).expect("prefund rest");
            set_out_amount(&mut e, rest, s.custody - used);
            let k = exit_cust(&e);
            set_out_amount(&mut e, k, amt + used);
            edit_exit(&mut e, i, |x| x.custody = amt + used);
            r.bad(&e, i);
        }
        // NIF17 sell-first: the prefund custody holds less than pre(n) on a continuing fill (the matcher tops it up)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIF17 sell prefund short of pre(n)");
            let i = ein(&e);
            let s = ist(&e, i);
            let n = arg(&e, i, NB);
            let pre = s.pre_of(n).unwrap();
            set_ist(&mut e, i, &IfdPairState { custody: pre - 1, ..s.clone() });
            let ci = cust_in(&e, TOKEN_B, E_ID);
            set_tin_amount(&mut e, ci, pre - 1);
            cont_state(&mut e, |c| c.custody = -1);
            // the prefund rest becomes the matcher's change of the extra B it brings
            let rest = entry_rest(&e, TOKEN_B).expect("prefund rest");
            rekey_out(&mut e, rest, MATCHER);
            set_out_amount(&mut e, rest, 1_000);
            take(&mut e, TOKEN_B, 161, 1_001 + (s.custody - pre) - (s.custody - pre));
            let c = cont(&e);
            e.fund_out(c, CARRIER as i64);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- outputs: continuation, exit, carriers, positions

#[test]
fn ifd_outputs() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NIO1 a continuing fill with a second output bound to the entry id
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO1 second output bound to the entry id (continuing)");
            let i = ein(&e);
            e.forge_bound(E_ID, i, KAS);
            r.bad(&e, i);
        }
        // NIO2 the continuation keeps amountLeft (not decreased by n)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO2 continuation keeps amountLeft");
            let i = ein(&e);
            let left = ist(&e, i).amount_left;
            cont_state(&mut e, |c| c.amount_left = left);
            r.bad(&e, i);
        }
        // NIO3 a terminating fill leaving an output bound to the entry id
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.close", "NIO3 output bound to the entry id after it terminates");
            let i = ein(&e);
            e.forge_bound(E_ID, i, KAS);
            r.bad(&e, i);
        }
        // NIO4 a continuing fill: the exit UTXO one sompi short of exitCarrier
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO4 exit short of exitCarrier (continuing)");
            let i = ein(&e);
            let xo = exit_out(&e);
            e.set_value(xo, e.value(xo) - 1);
            e.rebind_genesis(i as u16);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIO5 sell-first continuing fill: the A custody rest one sompi short of its carrier
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIO5 sell A rest carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_COV).expect("A rest");
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIO6 buy-first continuing fill: the escrow rest one sompi short of its carrier
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO6 buy escrow rest carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_B).expect("escrow rest");
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // PIO7 / NIO7 buy-first terminating fill with an escrow surplus: the return to the maker carries its carrier
        {
            let s = IfdPairState { amount_left: 4 * WHOLE, ..ifd_pair(MAKER_A, true, pa, pb) };
            let s = IfdPairState { custody: s.b_custody_needed().unwrap() + 3, ..s };
            r.ok(&edb("PIO7 buy close returning an escrow surplus", buy_fill(s.clone(), pa, pb, 4 * WHOLE, RATE, None)));
            let mut e = edb("NIO7 buy close: the escrow return short of its carrier", buy_fill(s, pa, pb, 4 * WHOLE, RATE, None));
            let i = ein(&e);
            let k = e
                .tok_outs_of(TOKEN_B)
                .into_iter()
                .find(|k| !e.out_state(*k).is_covenant_owned() && e.out_state(*k).owner() == pk(MAKER_A))
                .expect("escrow return");
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIO8 the last exit one sompi short of everything left
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.close", "NIO8 last exit short of the KAS left");
            let i = ein(&e);
            let xo = exit_out(&e);
            e.set_value(xo, e.value(xo) - 1);
            e.rebind_genesis(i as u16);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIO9 the exit custody is an unbound look-alike (its script is the token's, no covenant binding: the n of A go to
        // the matcher)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO9 exit custody an unbound look-alike");
            let i = ein(&e);
            let k = exit_cust(&e);
            let st = e.out_state(k);
            let p = prog(&e, TOKEN_COV);
            e.tok_out.remove(&k);
            e.tx.outputs[k].covenant = None;
            e.tx.outputs[k].script_public_key = st.spk_with(kob_protocol::artifacts::token_template(p));
            give(&mut e, TOKEN_COV, st.amount());
            r.bad(&e, i);
        }
        // NIO10 the escrow rest at another index (not the custody's)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIO10 escrow rest moved off the custody index");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_B).expect("escrow rest");
            let other = e.add_plain_output(e.value(k), MATCHER);
            e.swap_outputs(k, other);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- hostile state fields, cancel

#[test]
fn ifd_hostile() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NIH5 side 3 (neither ASK nor BID) read as a sell-first entry
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIH5 hostile side 3");
            let i = ein(&e);
            both(&mut e, i, |s| s.side = 3);
            r.bad(&e, i);
        }
        // NIH6 a negative scale of A (sell-first): the quotes turn negative, the exit keeps its custody, the prefund
        // custody grows by what the negative pre(n) says (the matcher pays it)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIH6 hostile negative scale of A");
            let i = ein(&e);
            let s = ist(&e, i);
            let n = arg(&e, i, NB);
            let d = -s.a_scale;
            let used = qof(n, s.prefund, d, d - 1);
            let xc = out_amount(&e, exit_cust(&e));
            let amt = xc - used;
            assert!(amt >= qof(n, s.price, d, d - 1));
            both(&mut e, i, |x| x.a_scale = d);
            set_int(&mut e, i, AMT, amt);
            cont_state(&mut e, |c| c.custody = s.custody - used);
            let rest = entry_rest(&e, TOKEN_B).expect("prefund rest");
            let old = out_amount(&e, rest);
            set_out_amount(&mut e, rest, s.custody - used);
            take(&mut e, TOKEN_B, 161, s.custody - used - old);
            r.bad(&e, i);
        }
        // NIH7k / NIH8k a family code of 3 (read as KCC-20) for the KCC-20 token: A on KCC/KRON, B on KRON/KCC
        if pa.family() == kob_protocol::Family::Kcc20 {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIH7k hostile aFamily 3");
            let i = ein(&e);
            both(&mut e, i, |s| s.a_family = 3);
            r.bad(&e, i);
        }
        if pb.family() == kob_protocol::Family::Kcc20 {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIH8k hostile bFamily 3");
            let i = ein(&e);
            both(&mut e, i, |s| s.b_family = 3);
            r.bad(&e, i);
        }
        // NIC1 cancel with a non-ALL sighash, NIC2 cancel signed by another key
        {
            let mut e = ed_of("NIC1 cancel with SIGHASH_NONE", cancel_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb, vec![]));
            let i = ein(&e);
            e.sighash_types.insert(i, 2);
            r.bad(&e, i);
            let mut e = ed_of("NIC2 cancel signed by another key", cancel_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb, vec![]));
            e.sign_as(i, MAKER_B);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- update rules

#[test]
fn ifd_update_rules() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let upd = |name: &str, buy: bool| ed_of(name, update_entry(stop_entry(pa, pb, buy, 0), pa, pb, 1));
        // NIU1 an update of a limit entry (entryStop 0): the keeper takes keeperTip for nothing
        {
            let mut e = upd("NIU1 update of a limit entry", true);
            let i = ein(&e);
            both(&mut e, i, |s| s.entry_stop = 0);
            r.bad(&e, i);
        }
        // NIU2 / NIU3 the stop ordering at an update: buy stop above its limit, sell stop below it
        {
            let mut e = upd("NIU2 update: buy stop above its limit", true);
            let i = ein(&e);
            both(&mut e, i, |s| s.price = s.entry_stop - 1);
            r.bad(&e, i);
            let mut e = upd("NIU3 update: sell stop below its limit", false);
            let i = ein(&e);
            both(&mut e, i, |s| s.price = s.entry_stop + 1);
            r.bad(&e, i);
        }
        // NIU4 an update of an entry with nothing left (a repeating entry waiting for its exits)
        {
            let mut e = upd("NIU4 update of an entry with nothing left", true);
            let i = ein(&e);
            both(&mut e, i, |s| {
                s.amount_left = 0;
                s.rpt_amount = 1 + 4 * WHOLE;
            });
            r.bad(&e, i);
        }
        // NIU5 an update of an armed entry in its auction (restarts the auction at armed = 1)
        {
            let mut e = upd("NIU5 update of an armed entry", true);
            let i = ein(&e);
            let s = ist(&e, i);
            set_ist(&mut e, i, &IfdPairState { armed: NOW as i64 - 150, ..s });
            r.bad(&e, i);
        }
        // NIU6 a hostile negative keeperTip (the keeper funds the continuation)
        {
            let mut e = upd("NIU6 hostile negative keeperTip", true);
            let i = ein(&e);
            let kt = ist(&e, i).keeper_tip;
            both(&mut e, i, |s| s.keeper_tip = -1);
            let c = cont(&e);
            e.fund_out(c, kt + 1);
            r.bad(&e, i);
        }
        // NIU7 an update before activeFrom
        {
            let mut e = upd("NIU7 update before activeFrom", true);
            let i = ein(&e);
            both(&mut e, i, |s| s.active_from = NOW as i64 + 1);
            r.bad(&e, i);
        }
        // NIU8 a terminating stop fill run with upd = 1 (an update that fills: the escrow custody is not spent and stays
        // behind, orphaned, owned by the terminated entry; the matcher pays the B the entry releases). The entry's custody
        // state equals the release so nothing is left to return (an update has no custody index).
        {
            let mut e = ed_shape(pa, pb, "pairgrid.ifd.bid.arm1.close", "NIU8 fill with upd = 1 (fill disguised as an update)");
            let i = ein(&e);
            set_int(&mut e, i, UPD, 1);
            let amt = arg(&e, i, AMT);
            let s = ist(&e, i);
            set_ist(&mut e, i, &IfdPairState { custody: amt, ..s });
            let ci = cust_in(&e, TOKEN_B, E_ID);
            rekey_tin(&mut e, ci, MATCHER);
            let ret = e
                .tok_outs_of(TOKEN_B)
                .into_iter()
                .find(|k| !e.out_state(*k).is_covenant_owned() && e.out_state(*k).owner() == pk(MAKER_A))
                .expect("escrow return");
            rekey_out(&mut e, ret, MATCHER);
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- refund rules

#[test]
fn ifd_refund_rules() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let rf = |name: &str| ed_of(name, refund_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb));
        // NIR1 a hostile negative refundTip (the keeper adds to the maker's KAS)
        {
            let mut e = rf("NIR1 hostile negative refundTip");
            let i = ein(&e);
            let rt = ist(&e, i).refund_tip;
            both(&mut e, i, |s| s.refund_tip = -1);
            e.fund_out(i, rt + 1);
            r.bad(&e, i);
        }
        // NIR2 / NIR3 the A / B custody returned one sompi short of its carrier
        {
            let mut e = rf("NIR2 refund: the A return short of its carrier");
            let i = ein(&e);
            let k = arg(&e, i, AIN) as usize;
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
            let mut e = rf("NIR3 refund: the B return short of its carrier");
            let k = arg(&e, i, BIN) as usize;
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIR4 the refund's KAS at output i to the keeper
        {
            let mut e = rf("NIR4 refund KAS to the keeper");
            let i = ein(&e);
            e.tx.outputs[i].script_public_key = kob_protocol::script::p2pk_spk(&pk(KEEPER));
            r.bad(&e, i);
        }
    });
}

// ---------------------------------------------------------------- the custody marks (heldGx) and the template sources (pinTok)

#[test]
fn ifd_custody_marks() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // the buy-first escrow (token B): `k` scenarios where B is KCC-20, `r` where it is KRON
        let f = ph::fam_tag(pb);
        let base = |name: &str| ed_shape(pa, pb, "pair.ifd.bid.cont", name);
        // NIH1 the escrow custody input is owned by another covenant (the ask of A in the transaction)
        {
            let mut e = base(&format!("NIH1{f} escrow custody owned by another covenant"));
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_B, E_ID);
            let mut st = tin(&e, ci);
            fx::set_owner(&mut st, cov(0x92));
            set_tin(&mut e, ci, st);
            r.bad(&e, i);
        }
        // NIH2 the escrow custody under another owner type: KCC-20 borrowing enabled / KRON id_type 3 (address presence)
        {
            let mut e = base(&format!("NIH2{f} escrow custody under another owner type"));
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_B, E_ID);
            let st = ph::map_tok(tin(&e, ci), |k| Kcc20State { borrow_scheme: 1, ..k }, |k| KronState { id_type: 3, ..k });
            set_tin(&mut e, ci, st);
            r.bad(&e, i);
        }
        // NIH3r a KRON escrow custody flagged as a minter
        if pb.family() == kob_protocol::Family::Kron {
            let mut e = base("NIH3r escrow custody flagged as a minter");
            let i = ein(&e);
            let ci = cust_in(&e, TOKEN_B, E_ID);
            let st = ph::map_tok(tin(&e, ci), |k| k, |k| KronState { is_minter: 1, ..k });
            set_tin(&mut e, ci, st);
            r.bad(&e, i);
        }
    });
    // A and B on the same program (KCC-20 / KCC-20): only the covenant ids tell the tokens apart
    let kk = [family_mixes()[0]];
    each(&subs, &kk, |r, pa, pb| {
        // PIH4 / NIH4 a sell-first entry selling out with its prefund equal to its A: the B custody named at the A custody
        // input (the real prefund custody stays behind; the matcher pays the B)
        let s = IfdPairState { amount_left: 4 * WHOLE, custody: 4 * WHOLE, ..ifd_pair(MAKER_A, false, pa, pb) };
        let p = s.price;
        r.ok(&edb("PIH4 sell-first sell-out with a prefund equal to its A", sell_fill(s.clone(), pa, pb, 4 * WHOLE, p, None)));
        {
            let mut e = edb("NIH4 the B custody named at the A custody input", sell_fill(s, pa, pb, 4 * WHOLE, p, None));
            let i = ein(&e);
            let ai = arg(&e, i, AIN);
            let bi = cust_in(&e, TOKEN_B, E_ID);
            set_int(&mut e, i, BIN, ai);
            rekey_tin(&mut e, bi, MATCHER);
            r.bad(&e, i);
        }
        // NIP1 the A template source (aTplIn) is an input of B (same program: the bytes are identical)
        {
            let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIP1 A template source is a B input");
            let i = ein(&e);
            let bi = cust_in(&e, TOKEN_B, E_ID);
            set_int(&mut e, i, ATPL, bi as i64);
            r.bad(&e, i);
        }
    });
    each(&subs, &mixed(), |r, pa, pb| {
        // NIP2 the A template source is a planted A-covenant UTXO whose redeem script is a look-alike (other template
        // bytes); the exit custody is written on those bytes. The A program itself refuses such an output (inputOnly).
        let e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIP2 A template source a planted look-alike");
        let i = ein(&e);
        let mut e = e;
        let j = e.add_p2pk_input(MATCHER, 0xe7);
        set_int(&mut e, i, ATPL, j as i64);
        let k = exit_cust(&e);
        let tpl = kob_protocol::artifacts::token_template(prog(&e, TOKEN_COV));
        let (pre, suf) = (tpl.prefix.len(), tpl.suffix.len());
        let stt = e.out_state(k);
        let len = stt.encode().len();
        let xo = exit_out(&e);
        let rs = ph::fake_rs(pre, &stt.encode(), suf);
        r.bad_patched(&e, i, &move |tx, en| {
            ph::plant_input(tx, en, j, TOKEN_COV, &rs, true);
            // the exit custody written on the look-alike bytes, owned by the finished exit id
            let xid = tx.outputs[xo].covenant.expect("exit binding").covenant_id.as_bytes();
            let mut st = stt.clone();
            fx::set_owner(&mut st, xid);
            let (fp, fs) = (rs[..pre].to_vec(), rs[pre + len..].to_vec());
            tx.outputs[k].script_public_key = kob_protocol::script::p2sh_spk(&[fp.as_slice(), &st.encode(), fs.as_slice()].concat());
        });
    });
}

// ---------------------------------------------------------------- the stray scan, slot by slot

/// A refund of a buy-first entry (inputs: the entry, its B escrow, the keeper's KAS) with a stray of `tok` owned by the
/// entry at slot `slot` of the token's inputs (key-held fillers of the matcher before it); every token in goes to the
/// matcher.
fn stray_at(name: &str, pa: TemplateId, pb: TemplateId, tok: [u8; 32], slot: usize) -> Ed {
    let mut e = ed_of(name, refund_entry(ifd_pair(MAKER_A, true, pa, pb), pa, pb));
    e.programs.entry(tok).or_insert(if tok == TOKEN_COV { pa } else { pb });
    let have = (0..e.plans.len())
        .filter(|i| ph::cov_of(&e.entries[*i]) == Some(tok) && !matches!(e.plans[*i], SigPlan::Entry { .. } | SigPlan::P2pk { .. }))
        .count();
    assert!(slot >= have, "{name}: slot {slot} < {have} inputs of the token already there");
    for j in have..slot {
        take(&mut e, tok, 170 + j as u8, 1);
    }
    stray(&mut e, tok, 190, 7, E_ID);
    give(&mut e, tok, (slot - have) as i64 + 7);
    e
}

#[test]
fn ifd_stray_slots() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NIS0..NIS3 a stray of A at slot 0..3 (both families: A is KCC-20 on one mixed pair, KRON on the other)
        for slot in 0..4 {
            let e = stray_at(&format!("NIS{slot} stray of A at token-input slot {slot}"), pa, pb, TOKEN_COV, slot);
            let i = ein(&e);
            r.bad(&e, i);
        }
        // NIS4..NIS7 a stray of the KCC-20 token at slot 4..7 (a KRON program carries at most 4 inputs of its token)
        let kcc = if pa.family() == kob_protocol::Family::Kcc20 { TOKEN_COV } else { TOKEN_B };
        for slot in 4..8 {
            let e = stray_at(&format!("NIS{slot} stray of the KCC-20 token at token-input slot {slot}"), pa, pb, kcc, slot);
            let i = ein(&e);
            r.bad(&e, i);
        }
    });
    // NIS8 nine inputs of a 16-input KCC-20 program: the scan covers eight, so a ninth input fails the scan bound
    let wide = [(TemplateId::Kcc20Ref16x16, KronToken2433)];
    each(&subs, &wide, |r, pa, pb| {
        let e = stray_at("NIS8 stray of A at token-input slot 8 (a 16-input program)", pa, pb, TOKEN_COV, 8);
        let i = ein(&e);
        r.bad(&e, i);
        // NIS9 the same nine A inputs without any stray: refused too (the bound is unconditional: a 16-input program can
        // carry at most eight inputs of a token into an entry spend; fail closed)
        let mut e = ed_of("NIS9 nine A inputs, no stray (a 16-input program)", refund_entry(ifd_pair(MAKER_A, true, pa, pb), pa, pb));
        e.programs.entry(TOKEN_COV).or_insert(pa);
        for j in 0..9 {
            take(&mut e, TOKEN_COV, 170 + j as u8, 1);
        }
        give(&mut e, TOKEN_COV, 9);
        let i = ein(&e);
        r.bad(&e, i);
    });
}

// ---------------------------------------------------------------- stop-entry evidence battery (both sides, both modes)

/// Evidence legs as [`ev_legs`] with the quotes of the A leg (`a`: KAS per whole A, or the pair price in mode 1) and the
/// B leg (`b`, mode 0) set.
fn ev_legs_px(pa: TemplateId, pb: TemplateId, mode: u8, fell: bool, a: i64, b: i64) -> Vec<Leg> {
    match (mode, fell) {
        (0, true) => vec![
            kask_units(TOKEN_COV, pa, MAKER_C, a, cov(0x93), 120, WHOLE),
            kbid_units(TOKEN_B, pb, MAKER_B, b, cov(0x94), 122, WHOLE),
            kbid_units(TOKEN_COV, pa, 24, P260, cov(0x95), 124, WHOLE),
            kask_units(TOKEN_B, pb, 25, P250, cov(0x96), 126, WHOLE),
        ],
        (0, false) => vec![
            kbid_units(TOKEN_COV, pa, MAKER_B, a, cov(0x93), 120, WHOLE),
            kask_units(TOKEN_B, pb, MAKER_C, b, cov(0x94), 122, WHOLE),
            kask_units(TOKEN_COV, pa, 24, P250, cov(0x95), 124, WHOLE),
            kbid_units(TOKEN_B, pb, 25, P260, cov(0x96), 126, WHOLE),
        ],
        (1, true) => vec![
            pair_leg(pair(MAKER_C, true, pa, pb, a), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
        _ => vec![
            pair_leg(pair(MAKER_C, false, pa, pb, a), pa, pb, cov(0x93), 120, 1),
            pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
        ],
    }
}
/// An update arming the stop entry `s` next to `legs` (evidence leg 0, and leg 1 as the B leg in mode 0).
fn update_with(s: IfdPairState, legs: Vec<Leg>, mode: u8) -> Batch {
    let mut b = route(legs);
    b.updates.push(BatchUpdate {
        order: order(60, ifd_value(&s), E_ID, 1_000, AnyState::KobIfdPair(s)),
        evidence: 0,
        evidence_b: (mode == 0).then_some(1),
        take: None,
    });
    b
}
/// Applies `f` to the KAS-book / pair order state of the evidence input of covenant `c`.
fn set_ev(e: &mut Ed, c: [u8; 32], f: &dyn Fn(&mut AnyState)) {
    let vi = e.input_of_cov(c);
    let (t, st) = e.entry_state(vi);
    let mut any = AnyState::decode(t, &st).unwrap();
    f(&mut any);
    e.set_entry_state(vi, any.encode());
}
fn ev_slope(s: &mut AnyState) {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.slope = 1,
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.slope = 1,
        AnyState::KobPair(x) => x.slope = 1,
        _ => panic!("not an evidence order"),
    }
}
fn ev_scale(s: &mut AnyState) {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.scale = 100,
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.scale = 100,
        AnyState::KobPair(x) => {
            x.s_scale = 100;
            x.t_scale = 100;
        }
        _ => panic!("not an evidence order"),
    }
}
fn ev_token(s: &mut AnyState) {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.token_cov_id = [0x72; 32],
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.token_cov_id = [0x72; 32],
        AnyState::KobPair(x) => {
            if x.is_ask() {
                x.t_cov_id = [0x72; 32];
            } else {
                x.s_cov_id = [0x72; 32];
            }
        }
        _ => panic!("not an evidence order"),
    }
}
/// Grinds the signature of input `ai` (its maker's cancel) until its signature-script bytes [1..9) read as a positive
/// amount of at least `min` (the change shifts by one sompi per try; every try is a fresh signature).
fn grind(r: &Run, e: &mut Ed, ai: usize, min: i64) {
    for _ in 0..400 {
        let (tx, _) = e.finish(r.subs);
        let ss = &tx.inputs[ai].signature_script;
        let n = i64::from_le_bytes(ss[1..9].try_into().unwrap());
        if n >= min {
            return;
        }
        let c = e.change_out();
        let v = e.value(c);
        e.set_value(c, v - 1);
    }
    panic!("{}: no signature reads as a fill", e.name);
}
/// A token input of `tok` (not an order) other than `not`, preferring one owned by covenant `own` (None: any).
fn tok_in(e: &Ed, tok: [u8; 32], not: usize) -> Option<usize> {
    (0..e.plans.len()).find(|k| {
        *k != not && ph::cov_of(&e.entries[*k]) == Some(tok) && !matches!(e.plans[*k], SigPlan::Entry { .. } | SigPlan::P2pk { .. })
    })
}

fn ev_battery(r: &Run, pa: TemplateId, pb: TemplateId) {
    for buy in [true, false] {
        let fell = !buy;
        let sd = if buy { "b" } else { "a" };
        let s0 = stop_entry(pa, pb, buy, 0);
        for mode in [0u8, 1] {
            let x = |id: &str| format!("{id}{sd}{mode}");
            let base = |name: &str| ed_of(name, update_entry(s0.clone(), pa, pb, mode));
            let (a_mk, b_mk) = if buy { (MAKER_B, MAKER_C) } else { (MAKER_C, MAKER_B) };
            // ---- the A leg (mode 0) / the pair order (mode 1): read by rd()
            // NE01 unfilled: the evidence pushes n = 0 (its refund / update encoding; the order's own script refuses it
            // here, so only the entry input is judged). The entry's minTouch is 0 so only `n > 0` stands.
            {
                let mut e =
                    base(&format!("{} {} evidence pushes n = 0 (minTouch 0)", x("NE01"), if mode == 0 { "A" } else { "pair" }));
                let i = ein(&e);
                both(&mut e, i, |s| s.min_touch = 0);
                let ai = e.input_of_cov(cov(0x93));
                e.set_arg(ai, 0, ph::nb(0));
                r.bad(&e, i);
            }
            // NE03 cancelled by its maker, the signature ground so that bytes [1..9) read as a fill
            {
                let mut e = base(&format!(
                    "{} {} evidence cancelled, signature read as a fill",
                    x("NE03"),
                    if mode == 0 { "A" } else { "pair" }
                ));
                let i = ein(&e);
                let ai = e.input_of_cov(cov(0x93));
                let mk = if mode == 0 { a_mk } else { MAKER_C };
                e.set_entry(ai, "cancel", vec![Arg::Sig(pk(mk))]);
                grind(r, &mut e, ai, WHOLE);
                r.bad(&e, i);
            }
            // NE05 decaying (slope 1)
            {
                let mut e = base(&format!("{} {} evidence decaying", x("NE05"), if mode == 0 { "A" } else { "pair" }));
                let i = ein(&e);
                set_ev(&mut e, cov(0x93), &ev_slope);
                r.bad(&e, i);
            }
            // NE07 below minTouch (minTouch one above the fill; mode 0 also moves the stop so the B threshold stays met)
            {
                let mut e = base(&format!("{} {} evidence below minTouch", x("NE07"), if mode == 0 { "A" } else { "pair" }));
                let i = ein(&e);
                both(&mut e, i, |s| {
                    s.min_touch = WHOLE + 1;
                    if mode == 0 {
                        s.entry_stop = RATE - 1;
                    }
                });
                r.bad(&e, i);
            }
            // NE09 another scale, NE11 another token
            {
                let mut e = base(&format!("{} {} evidence of another scale", x("NE09"), if mode == 0 { "A" } else { "pair" }));
                let i = ein(&e);
                set_ev(&mut e, cov(0x93), &ev_scale);
                r.bad(&e, i);
                let mut e = base(&format!("{} {} evidence of another token", x("NE11"), if mode == 0 { "A" } else { "pair" }));
                set_ev(&mut e, cov(0x93), &ev_token);
                r.bad(&e, i);
            }
            // NE13 wrong side: the order's own counterparty (mode 0: the opposite-side KAS-book orders of A and B; mode 1:
            // the opposite pair order)
            {
                let mut e = base(&format!("{} evidence on the wrong side (the own counterparty)", x("NE13")));
                let i = ein(&e);
                if mode == 0 {
                    let (ea, eb) = (e.input_of_cov(cov(0x95)), e.input_of_cov(cov(0x96)));
                    set_int(&mut e, i, EVA, ea as i64);
                    set_int(&mut e, i, EVB, eb as i64);
                    let tkc = if buy { cov(0x95) } else { cov(0x96) };
                    let tok = if buy { TOKEN_COV } else { TOKEN_B };
                    let t = cust_in(&e, tok, tkc);
                    set_int(&mut e, i, TK, t as i64);
                } else {
                    let ea = e.input_of_cov(cov(0x94));
                    set_int(&mut e, i, EVA, ea as i64);
                    let t = cust_in(&e, if buy { TOKEN_COV } else { TOKEN_B }, cov(0x94));
                    set_int(&mut e, i, TK, t as i64);
                }
                r.bad(&e, i);
            }
            // NE14 / NE15 / NE16 the evidence index at the entry itself, at a token input, at a P2PK input
            {
                let mut e = base(&format!("{} evidence at the entry itself", x("NE14")));
                let i = ein(&e);
                set_int(&mut e, i, EVA, i as i64);
                r.bad(&e, i);
                let mut e = base(&format!("{} evidence at a token input", x("NE15")));
                let t = arg(&e, i, TK);
                set_int(&mut e, i, EVA, t);
                r.bad(&e, i);
                let mut e = base(&format!("{} evidence at a P2PK input", x("NE16")));
                let j = e.add_p2pk_input(MATCHER, 0xea);
                set_int(&mut e, i, EVA, j as i64);
                r.bad(&e, i);
            }
            // NE17 the implied-rate edge: exactly at the stop accepted (PE17), one unit beyond refused
            {
                let (ok_a, bad_a, b) = match (mode, buy) {
                    (0, true) => (P250, P250 - 1, P250),
                    (0, false) => (P260, P260 + 1, P260),
                    (_, true) => (RATE, RATE - 1, 0),
                    (_, false) => (RATE, RATE + 1, 0),
                };
                let s = s0.clone();
                r.ok(&edb(
                    &format!("{} evidence exactly at the stop", x("PE17")),
                    update_with(s.clone(), ev_legs_px(pa, pb, mode, fell, ok_a, b), mode),
                ));
                // the evidence exactly at the stop, the stop then moved one unit past it (minTouch 999 keeps the mode-0 B
                // threshold at 1,000)
                let _ = bad_a;
                let mut e = edb(
                    &format!("{} evidence one unit past the stop", x("NE17")),
                    update_with(s, ev_legs_px(pa, pb, mode, fell, ok_a, b), mode),
                );
                let i = ein(&e);
                both(&mut e, i, |t| {
                    t.entry_stop = if buy { RATE + 1 } else { RATE - 1 };
                    t.min_touch = WHOLE - 1;
                });
                r.bad(&e, i);
            }
            // ---- the custody of an ask / pair evidence (tk): the A ask (sell, mode 0), the pair (mode 1) are read by rd(),
            // the B ask (buy, mode 0) by evLeg()
            {
                // NE18 tk is a token input of the other token
                let mut e = base(&format!("{} evidence custody of another token", x("NE18")));
                let i = ein(&e);
                let t = arg(&e, i, TK) as usize;
                let tok = ph::cov_of(&e.entries[t]).unwrap();
                let other = if tok == TOKEN_COV { TOKEN_B } else { TOKEN_COV };
                let w = tok_in(&e, other, usize::MAX).expect("a token input of the other token");
                set_int(&mut e, i, TK, w as i64);
                r.bad(&e, i);
                // NE19 tk is a key-held UTXO of the right token (not owned by the evidence order)
                let mut e = base(&format!("{} evidence custody not owned by the evidence", x("NE19")));
                let w = take(&mut e, tok, 0xe1, 5);
                give(&mut e, tok, 5);
                set_int(&mut e, i, TK, w as i64);
                r.bad(&e, i);
            }
            if mode == 0 {
                // ---- the B leg: read by evLeg()
                // NE02 the B evidence pushes n = 0 (minTouch 0: the B threshold is 0)
                {
                    let mut e = base(&format!("{} B evidence pushes n = 0 (minTouch 0)", x("NE02")));
                    let i = ein(&e);
                    both(&mut e, i, |s| s.min_touch = 0);
                    let bi = e.input_of_cov(cov(0x94));
                    e.set_arg(bi, 0, ph::nb(0));
                    r.bad(&e, i);
                }
                // NE04 the B evidence cancelled, its signature read as a fill
                {
                    let mut e = base(&format!("{} B evidence cancelled, signature read as a fill", x("NE04")));
                    let i = ein(&e);
                    let bi = e.input_of_cov(cov(0x94));
                    e.set_entry(bi, "cancel", vec![Arg::Sig(pk(b_mk))]);
                    grind(r, &mut e, bi, WHOLE);
                    r.bad(&e, i);
                }
                // NE06 B decaying, NE10 B another scale, NE12 B another token
                {
                    let mut e = base(&format!("{} B evidence decaying", x("NE06")));
                    let i = ein(&e);
                    set_ev(&mut e, cov(0x94), &ev_slope);
                    r.bad(&e, i);
                    let mut e = base(&format!("{} B evidence of another scale", x("NE10")));
                    set_ev(&mut e, cov(0x94), &ev_scale);
                    r.bad(&e, i);
                    let mut e = base(&format!("{} B evidence of another token", x("NE12")));
                    set_ev(&mut e, cov(0x94), &ev_token);
                    r.bad(&e, i);
                }
                // NE08 the B evidence one base unit below its threshold ceil(minTouch * stop / scale(A)) (the stop moved up by
                // one: the threshold becomes 1,001, the fill is 1,000)
                {
                    let mut e = base(&format!("{} B evidence below the B threshold", x("NE08")));
                    let i = ein(&e);
                    both(&mut e, i, |s| s.entry_stop = RATE + 1);
                    r.bad(&e, i);
                }
            } else {
                // NE21 evMode 2 (read as a pair order when unchecked)
                let mut e = base(&format!("{} evMode 2", x("NE21")));
                let i = ein(&e);
                set_int(&mut e, i, EVM, 2);
                r.bad(&e, i);
            }
        }
    }
    // NE22b0 a buy stop armed by an ask of B at price 0 (b = 0: any quote of A reaches ceil(stop * 0 / scale(B)) = 0)
    let s = stop_entry(pa, pb, true, 0);
    // built at price 1 (tip 0), then the ask's price set to 0 (it then owes its maker nothing: the 1 sompi paid is surplus)
    let mut legs = ev_legs_px(pa, pb, 0, false, P260, 1);
    if let Leg::Ask { order, .. } = &mut legs[1] {
        order.state.tip = 0;
    }
    let mut e = edu("NE22b0 B ask at price 0 as the B leg", update_with(s, legs, 0));
    set_ev(&mut e, cov(0x94), &|x| match x {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => a.price = 0,
        _ => panic!("the B leg is an ask"),
    });
    let i = ein(&e);
    r.bad(&e, i);
}

#[test]
fn ifd_ev_battery() {
    let subs = Subs::compile();
    each(&subs, &mixed(), ev_battery);
}

// ---------------------------------------------------------------- merge: the exit read, the amounts, the custodies

/// The fixtures' `rearm` with the entry built from `base` (a repeating entry with `entry_left` whole A left).
fn rearm_with(base: IfdPairState, pa: TemplateId, pb: TemplateId, held: i64, n: i64, entry_left: i64) -> Batch {
    let buy = base.is_buy_first();
    let mut e = IfdPairState { rpt_amount: 1 + 20 * WHOLE, amount_left: entry_left * WHOLE, ..base.clone() };
    e.custody = if buy {
        e.spend(e.amount_left, e.price).unwrap()
    } else if entry_left > 0 {
        e.b_custody_needed().unwrap()
    } else {
        0
    };
    let x = booked_exit(&base, held);
    let (stok, sp) = s_of(x.is_ask(), pa, pb);
    let xcust = tutxo(sp, stok, 81, x.custody, XE_ID, true);
    let a_custody = (!buy && e.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, 83, e.amount_left, E_ID, true));
    let b_custody = (e.custody > 0).then(|| tutxo(pb, TOKEN_B, 84, e.custody, E_ID, true));
    let merge = kob_protocol::build::PairEntryMerge { entry: order(82, ifd_value(&e), E_ID, 1_000, e), a_custody, b_custody };
    let exit_leg = Leg::CondPair {
        order: order(80, PEC as u64, XE_ID, 1_000, x.clone()),
        custody: xcust,
        amount: n * WHOLE,
        leg: 0,
        evidence: None,
        evidence_b: None,
        t: None,
        merge: Some(merge),
    };
    let m = n * WHOLE;
    if buy {
        let proceeds = x.rpt_proceeds(m).unwrap();
        let t_out = x.t_out_min(m, x.tp_price).unwrap().max(proceeds + 1);
        route(vec![
            exit_leg,
            kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 74, m),
            kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 76, t_out),
        ])
    } else {
        let proceeds = x.rpt_proceeds(m).unwrap();
        let back = x.rpt_back(m).unwrap();
        let floor = x.s_out(m, x.tp_price).unwrap();
        let s_out = if m < x.amount_left { floor.min(proceeds - 1) } else { floor.min(x.custody - back - 1) };
        route(vec![
            exit_leg,
            kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 74, s_out),
            kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 76, m),
        ])
    }
}
/// The maker's token output of `tok` (owned by MAKER_A's key).
fn maker_out(e: &Ed, tok: [u8; 32]) -> usize {
    e.tok_outs_of(tok)
        .into_iter()
        .find(|k| !e.out_state(*k).is_covenant_owned() && e.out_state(*k).owner() == pk(MAKER_A))
        .unwrap_or_else(|| panic!("{}: no maker output", e.name))
}
/// Applies `f` to the exit input's state (a booked exit spent in a re-arm).
fn restate_x(e: &mut Ed, f: impl Fn(&mut CondPairState)) {
    let x = xin(e);
    let mut s = xst(e, x);
    f(&mut s);
    e.set_entry_state(x, s.encode());
}
/// The exit's continuation output (a partial re-arm), if any.
fn xcont(e: &Ed) -> Option<usize> {
    e.bound_to(XE_ID).into_iter().find(|k| !e.is_tok_out(*k))
}
/// Replaces the first push of input `i`'s finished signature script (`0x08` + 8 bytes) by `with` (the rest kept).
fn first_push(r: &Run, e: &mut Ed, i: usize, with: &[u8]) {
    let (tx, _) = e.finish(r.subs);
    let ss = &tx.inputs[i].signature_script;
    assert_eq!(ss[0], 0x08, "{}: input {i} does not start with an 8-byte push", e.name);
    let mut v = with.to_vec();
    v.extend_from_slice(&ss[9..]);
    e.raw_sigscripts.insert(i, v);
}

#[test]
fn ifd_merge_rules() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let bsell = |name: &str| ed_shape(pa, pb, "pair.rearm.bid.sellout", name);
        let asell = |name: &str| ed_shape(pa, pb, "pair.rearm.ask.sellout", name);
        // NIX1 a look-alike exit: another tpPrice (outside the mutable payloads of the committed exit state)
        {
            let mut e = bsell("NIX1 look-alike exit: another tpPrice");
            let i = ein(&e);
            restate_x(&mut e, |x| x.tp_price -= 1);
            r.bad(&e, i);
        }
        // NIX2 a look-alike exit: another rptPrice (the matcher pays the difference)
        {
            let mut e = bsell("NIX2 look-alike exit: another rptPrice");
            let i = ein(&e);
            let (x0, m) = (xst(&e, xin(&e)), arg(&e, xin(&e), C_NB));
            let p1 = x0.rpt_price - 1;
            let d = qof(m, x0.rpt_price, x0.s_scale, x0.s_scale - 1) - qof(m, p1, x0.s_scale, x0.s_scale - 1);
            restate_x(&mut e, |x| x.rpt_price = p1);
            let k = maker_out(&e, TOKEN_B);
            e.add_out_amount(k, d);
            take(&mut e, TOKEN_B, 161, d);
            r.bad(&e, i);
        }
        // NIX4 the committed exit quotes at another scale of A (999): the exit's re-arm rounds at its scale, the entry at
        // its own (the maker gets less than its exit's own take-profit)
        {
            let mut e = bsell("NIX4 committed exit at another scale of A");
            let i = ein(&e);
            let mut ex = ist(&e, i).exit().unwrap();
            ex.s_scale = 999;
            let commit = IfdPairState::commit_exit(&ex);
            both(&mut e, i, |s| s.exit_state = commit.clone());
            restate_x(&mut e, |x| x.s_scale = 999);
            let x = xin(&e);
            let xs = xst(&e, x);
            let m = arg(&e, x, C_NB);
            set_int(&mut e, x, C_TOUT, qof(m, xs.tp_price, 999, 998));
            r.bad(&e, i);
        }
        // NIX5 the exit's first push is a 9-byte push whose first 8 bytes are m (not the 0x08 push of its settle; the
        // exit's own byte[8] argument refuses it too)
        {
            let mut e = bsell("NIX5 exit's first push a 9-byte push of m");
            let i = ein(&e);
            let x = xin(&e);
            let m = arg(&e, x, C_NB);
            let mut p = vec![0x09u8];
            p.extend_from_slice(&m.to_le_bytes());
            p.push(0);
            first_push(r, &mut e, x, &p);
            r.bad(&e, i);
        }
        // NIX6 the exit's first push minimal (0x02 + 2 bytes): the bytes [1..9) no longer read as m
        {
            let mut e = bsell("NIX6 exit's first push minimal");
            let i = ein(&e);
            let x = xin(&e);
            let m = arg(&e, x, C_NB);
            let b = m.to_le_bytes();
            first_push(r, &mut e, x, &[0x02, b[0], b[1]]);
            r.bad(&e, i);
        }
        // NIX7 / NIX7e the entry merges m = n - 1 whole A while the exit trades n: the entry's escrow grows by budget(m)
        // only, the exit's budget(n) rest is the matcher's. Judged at the exit (NIX7: its check of the entry's push) and at
        // the entry (NIX7e: its check of the exit's push).
        for (id, judge_exit) in [("NIX7", true), ("NIX7e", false)] {
            let mut e = bsell(&format!("{id} entry merges m != the exit's n"));
            let i = ein(&e);
            let x = xin(&e);
            let n = arg(&e, x, C_NB);
            let m = n - WHOLE;
            let s = ist(&e, i);
            e.set_arg(i, NB, merge_nb(x as i64, m));
            let k = entry_rest(&e, TOKEN_B).expect("merged escrow");
            set_out_amount(&mut e, k, s.custody + s.merge_budget(m).unwrap());
            give(&mut e, TOKEN_B, s.merge_budget(n).unwrap() - s.merge_budget(m).unwrap());
            cont_state(&mut e, |c| c.amount_left = s.amount_left + m);
            cont_state(&mut e, |c| c.custody = s.custody + s.merge_budget(m).unwrap());
            r.bad(&e, if judge_exit { x } else { i });
        }
        // NIX11 the exit custody input xc named at the matcher's P2PK input (the entry's claim of the exit's KAS shrinks)
        {
            let mut e = bsell("NIX11 xc names a P2PK input");
            let i = ein(&e);
            let j = e.add_p2pk_input(MATCHER, 0xe3);
            set_int(&mut e, i, XC, j as i64);
            r.bad(&e, i);
        }
        // NIX9 xc names a genuine A UTXO owned by the exit (a stray of the exit) with one sompi of KAS (the claim shrinks
        // by the custody's carrier). The exit's stray guard is what refuses it: judged at the exit.
        {
            let mut e = bsell("NIX9 xc names a stray of the exit");
            let i = ein(&e);
            let x = xin(&e);
            let amt = xst(&e, x).custody;
            let st = cov_st(&e, TOKEN_COV, amt, XE_ID);
            let j = e.add_token_input(utxo(0xe4, 1, 1_000, Some(TOKEN_COV)), st, Wit::CovenantId);
            give(&mut e, TOKEN_COV, amt);
            set_int(&mut e, i, XC, j as i64);
            r.bad(&e, x);
        }
        // NIM5 a sell-out re-arm: the entry's continuation one sompi short of the exit's KAS it claims
        {
            let mut e = bsell("NIM5 sell-out re-arm: continuation short of the claimed KAS");
            let i = ein(&e);
            let c = cont(&e);
            e.set_value(c, e.value(c) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // NIM6 / NIM7 a merge into an existing custody: the A custody (sell-first) / the escrow (buy-first) short of its
        // carrier
        {
            let mut e = asell("NIM6 sell merge: A custody carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_COV).expect("A custody");
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
            let mut e = bsell("NIM7 buy merge: escrow carrier short");
            let i = ein(&e);
            let k = entry_rest(&e, TOKEN_B).expect("escrow");
            e.set_value(k, e.value(k) - 1);
            to_change(&mut e, 1);
            r.bad(&e, i);
        }
        // PIM8 / NIM8 a buy-first entry at price 1 re-armed (budget 4), then the same with price 0 and rptPrice 0 (budget 0:
        // the escrow does not grow, the maker's profit takes the 4)
        {
            let base = IfdPairState { price: 1, ..ifd_pair(MAKER_A, true, pa, pb) };
            r.ok(&edb("PIM8 buy-first entry at price 1 re-armed", rearm_with(base.clone(), pa, pb, 4, 4, 6)));
            let mut e = edb("NIM8 buy-first entry at price 0 re-armed (budget 0)", rearm_with(base, pa, pb, 4, 4, 6));
            let i = ein(&e);
            let s = ist(&e, i);
            let m = arg(&e, xin(&e), C_NB);
            let budget = s.merge_budget(m).unwrap();
            both(&mut e, i, |s| s.price = 0);
            restate_x(&mut e, |x| x.rpt_price = 0);
            let k = entry_rest(&e, TOKEN_B).expect("escrow");
            e.add_out_amount(k, -budget);
            cont_state(&mut e, |c| c.custody = s.custody);
            let mk = maker_out(&e, TOKEN_B);
            e.add_out_amount(mk, budget);
            r.bad(&e, i);
        }
        // NIM9 a sell-first merge of m = 0 next to the exit's take-profit of n (judged at the entry: its m > 0 check and its
        // check of the exit's push both refuse it)
        {
            let mut e = asell("NIM9 sell merge of m = 0 next to a take-profit");
            let i = ein(&e);
            let x = xin(&e);
            let n = arg(&e, x, C_NB);
            let s = ist(&e, i);
            e.set_arg(i, NB, merge_nb(x as i64, 0));
            // the custodies stay as they are: the n bought A and the prefund's back(n) go to the matcher
            let ka = entry_rest(&e, TOKEN_COV).expect("A custody");
            set_out_amount(&mut e, ka, s.amount_left);
            let kb = entry_rest(&e, TOKEN_B).expect("prefund");
            set_out_amount(&mut e, kb, s.custody);
            give(&mut e, TOKEN_COV, n);
            give(&mut e, TOKEN_B, s.pre_of(n).unwrap());
            cont_state(&mut e, |c| {
                c.amount_left = s.amount_left;
                c.custody = s.custody;
            });
            r.bad(&e, i);
        }
    });
    // NIX3 the committed exit is of the entry's own side: a buy-first entry (made from a sell-first re-arm) whose booked
    // exit is a BID (a partial re-arm: the exit continues)
    each(&subs, &mixed(), |r, pa, pb| {
        if !pair_scenarios(pa, pb).iter().any(|(n, _)| n == "pair.rearm.ask") {
            return;
        }
        let mut e = ed_shape(pa, pb, "pair.rearm.ask", "NIX3 committed exit of the entry's own side");
        let i = ein(&e);
        let x = xin(&e);
        let m = arg(&e, x, C_NB);
        let s = ist(&e, i);
        let xs = xst(&e, x);
        let back = xs.rpt_back(m).unwrap();
        let budget = s.merge_budget(m).unwrap();
        both(&mut e, i, |t| {
            t.side = SIDE_BID;
            t.prefund = 0;
        });
        cont_state(&mut e, |c| c.custody = s.custody + budget);
        // the exit books no prefund (rptPre 0): its custody rest keeps back(m)
        restate_x(&mut e, |t| t.rpt_pre = 0);
        let xc = xcont(&e).expect("exit continuation");
        let mut xcs = ost::<CondPairState>(&e, xc);
        xcs.rpt_pre = 0;
        xcs.custody += back;
        e.set_out_spk_state(xc, COND, &xcs.encode());
        let xr = e.tok_out_of(TOKEN_B, XE_ID);
        e.add_out_amount(xr, back);
        // the A custody is the matcher's (a buy-first entry holds none); the prefund custody is the escrow
        let ai = cust_in(&e, TOKEN_COV, E_ID);
        rekey_tin(&mut e, ai, MATCHER);
        let ao = entry_rest(&e, TOKEN_COV).expect("A custody");
        rekey_out(&mut e, ao, MATCHER);
        let bo = entry_rest(&e, TOKEN_B).expect("prefund");
        set_out_amount(&mut e, bo, s.custody + budget);
        let grow = (s.custody + budget) - (s.custody + s.pre_of(m).unwrap());
        take(&mut e, TOKEN_B, 162, grow + back);
        r.bad(&e, i);
    });
}

// ---------------------------------------------------------------- the re-arming exit (KobCondPair repeat rules)

#[test]
fn cond_rpt_rules() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        let bsell = |name: &str| ed_shape(pa, pb, "pair.rearm.bid.sellout", name);
        // NCR5 the booked exit's STOP leg (armed, sold out) next to its entry's merge: a stop-loss never re-arms (the
        // matcher would fund the budget; the maker gets the whole stop proceeds)
        {
            let mut e = bsell("NCR5 booked exit's stop fill next to its entry's merge");
            let x = xin(&e);
            restate_x(&mut e, |s| s.armed = NOW as i64 - 1_000);
            set_int(&mut e, x, C_LEG, 1);
            set_int(&mut e, x, C_T, NOW as i64);
            let t_out = arg(&e, x, C_TOUT);
            let mk = maker_out(&e, TOKEN_B);
            let d = t_out - out_amount(&e, mk);
            e.add_out_amount(mk, d);
            take(&mut e, TOKEN_B, 161, d);
            // without a re-arm the sold-out exit pays its KAS (UTXO and custody carrier) to the maker's output
            let xc = arg(&e, ein(&e), XC) as usize;
            let want = e.entries[x].amount + e.entries[xc].amount;
            let have = e.value(mk);
            e.fund_out(mk, want as i64 - have as i64);
            r.bad(&e, x);
        }
        // PCR4 / NCR4 a booked exit's update (arming its stop) alone, and next to a fill of its entry
        {
            let x = booked_exit(&ifd_pair(MAKER_A, true, pa, pb), 4);
            let ev = || {
                vec![
                    pair_leg(pair(MAKER_C, true, pa, pb, RATE - 110), pa, pb, cov(0x93), 120, 1),
                    pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
                ]
            };
            let upd = |b: &mut Batch, at: usize| {
                b.updates.push(BatchUpdate {
                    order: order(60, PEC as u64, XE_ID, 1_000, AnyState::KobCondPair(x.clone())),
                    evidence: at,
                    evidence_b: None,
                    take: None,
                });
            };
            let mut b = route(ev());
            upd(&mut b, 0);
            r.ok(&edb("PCR4 booked exit armed by an update (entry absent)", b));
            let mut b = ifd_bid_route(ifd_pair(MAKER_A, true, pa, pb), pa, pb, 4);
            b.legs.extend(ev());
            upd(&mut b, 3);
            if let Some(u) = b.updates.last_mut() {
                if let AnyState::KobCondPair(c) = &mut u.order.state {
                    c.parent = [0x55; 32];
                }
            }
            let mut e = edb("NCR4 booked exit's update next to a fill of its entry", b);
            let xi = xin(&e);
            restate_x(&mut e, |c| c.parent = E_ID);
            let xc = xcont(&e).expect("exit continuation");
            let mut cs = ost::<CondPairState>(&e, xc);
            cs.parent = E_ID;
            e.set_out_spk_state(xc, COND, &cs.encode());
            r.bad(&e, xi);
        }
        // NCR6 the committed take-profit equals the entry price: the re-arm leaves the maker a profit of 0 (a zero token
        // output; the KRON program refuses one too)
        {
            let mut e = bsell("NCR6 re-arm profit of 0");
            let i = ein(&e);
            let mut ex = ist(&e, i).exit().unwrap();
            ex.tp_price = RATE;
            let commit = IfdPairState::commit_exit(&ex);
            both(&mut e, i, |s| s.exit_state = commit.clone());
            restate_x(&mut e, |s| s.tp_price = RATE);
            let x = xin(&e);
            let m = arg(&e, x, C_NB);
            set_int(&mut e, x, C_TOUT, m);
            let mk = maker_out(&e, TOKEN_B);
            let old = out_amount(&e, mk);
            set_out_amount(&mut e, mk, 0);
            give(&mut e, TOKEN_B, old);
            r.bad(&e, x);
        }
        // NCR7 a look-alike booked exit whose custody exceeds its amountLeft by one, sold out in a re-arm: one A would be
        // left over (refused; the S return would also collide with the profit output at index i)
        {
            let mut e = bsell("NCR7 sold-out re-arm leaving custody behind");
            let x = xin(&e);
            let c = xst(&e, x).custody + 1;
            restate_x(&mut e, |s| s.custody = c);
            let ci = arg(&e, x, C_CUST) as usize;
            set_tin_amount(&mut e, ci, c);
            give(&mut e, TOKEN_COV, 1);
            r.bad(&e, x);
        }
        // NCR8 a sold-out re-arm: the maker's profit output one sompi short of its delivery carrier
        {
            let mut e = bsell("NCR8 sold-out re-arm: profit output carrier short");
            let x = xin(&e);
            let mk = maker_out(&e, TOKEN_B);
            e.set_value(mk, e.value(mk) - 1);
            to_change(&mut e, 1);
            r.bad(&e, x);
        }
        // NCR9 the entry's merge push as a 9-byte push carrying the merge argument (the entry's own byte[8] argument
        // refuses it too)
        {
            let mut e = bsell("NCR9 entry's merge push a 9-byte push");
            let i = ein(&e);
            let x = xin(&e);
            let Arg::Bytes(nbb) = e.args(i)[NB].clone() else { panic!("nb") };
            let mut p = vec![0x09u8];
            p.extend_from_slice(&nbb);
            p.push(0);
            first_push(r, &mut e, i, &p);
            r.bad(&e, x);
        }
        // NCR3 the entry pushes a positive nb (a fill argument) next to the exit's re-arm: never read as a merge
        {
            let mut e = bsell("NCR3 entry pushes a positive nb next to the re-arm");
            let i = ein(&e);
            let x = xin(&e);
            let n = arg(&e, x, C_NB);
            set_nb(&mut e, i, n);
            r.bad(&e, x);
        }
    });
}

// ---------------------------------------------------------------- the evidence read through tplState (look-alike, planted)

#[test]
fn ifd_ev_template() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NE23 the A evidence (an ask of A arming a sell stop, mode 0) is a look-alike template: a P2SH UTXO of the
        // evidence's covenant id whose redeem script carries the genuine state at the KobAsk offsets but other prefix /
        // suffix bytes; NE24 a planted non-P2SH UTXO of that id carrying the GENUINE KobAsk redeem bytes. Both push the
        // 8-byte fill amount first, like a real fill.
        for (id, look) in
            [("NE23 A evidence of a look-alike template", true), ("NE24 A evidence planted (not P2SH, genuine redeem bytes)", false)]
        {
            let e = ed_of(id, update_entry(stop_entry(pa, pb, false, 0), pa, pb, 0));
            let i = ein(&e);
            let ai = e.input_of_cov(cov(0x93));
            let (t, st) = e.entry_state(ai);
            let acov = ph::cov_of(&e.entries[ai]).unwrap();
            let tpl = kob_protocol::artifacts::template(t);
            let rs = if look {
                let mut rs = vec![kaspa_txscript::opcodes::codes::OpDrop];
                rs.extend(ph::fake_rs(tpl.prefix.len() - 1, &st, tpl.suffix.len()));
                rs
            } else {
                [tpl.prefix.as_slice(), &st, tpl.suffix.as_slice()].concat()
            };
            let mut ss = vec![0x08u8];
            ss.extend_from_slice(&WHOLE.to_le_bytes());
            ss.extend(kob_protocol::script::push_data(&rs));
            r.bad_patched(&e, i, &move |tx, en| {
                ph::plant_input(tx, en, ai, acov, &rs, look);
                if !look {
                    use kaspa_txscript::opcodes::codes::{Op2Drop, OpTrue};
                    let x = &en[ai];
                    en[ai] = kaspa_consensus_core::tx::UtxoEntry::new(
                        x.amount,
                        kaspa_consensus_core::tx::ScriptPublicKey::new(0, vec![Op2Drop, OpTrue].into()),
                        x.block_daa_score,
                        false,
                        x.covenant_id,
                    );
                }
                tx.inputs[ai].signature_script = ss.clone();
            });
        }
        // NE30 / NE31 the C1 fake quotes as mode-1 evidence: a pair ASK whose custody != amountLeft, an unfunded
        // pair BID (the carrier wall is a KobPair matter: a sold-out ASK owes no carrier). The fake pair order cannot be filled (judged at the fake order's input), so it
        // never arms the entry.
        {
            // NE30a1: a sold-out pair ASK of 1 whole A whose custody holds 2 (the extra A to the matcher)
            let ask1 = with_left(pair(MAKER_C, true, pa, pb, RATE - 10), 1);
            let legs = vec![
                pair_leg(ask1.clone(), pa, pb, cov(0x93), 120, 1),
                pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
            ];
            let mut e = edb(
                "NE30a1 sell-stop entry armed by a pair ASK whose custody != amountLeft",
                update_with(stop_entry(pa, pb, false, 0), legs, 1),
            );
            let vi = e.input_of_cov(cov(0x93));
            set_ev(&mut e, cov(0x93), &|x| {
                if let AnyState::KobPair(p) = x {
                    p.custody = 2 * WHOLE
                }
            });
            let ci = cust_in(&e, TOKEN_COV, cov(0x93));
            set_tin_amount(&mut e, ci, 2 * WHOLE);
            give(&mut e, TOKEN_COV, WHOLE);
            r.bad(&e, vi);
            // NE31b1: a sold-out pair BID of 1 whole A whose escrow is one unit short of its quote (the matcher adds it)
            let q = PairState::s_out(&pair(MAKER_C, false, pa, pb, RATE + 10), WHOLE, RATE + 10).unwrap();
            let bid1 = PairState { custody: q, ..with_left(pair(MAKER_C, false, pa, pb, RATE + 10), 1) };
            let legs = vec![
                pair_leg(bid1, pa, pb, cov(0x93), 120, 1),
                pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 122, 1),
            ];
            let mut e = edb("NE31b1 buy-stop entry armed by an unfunded pair BID", update_with(stop_entry(pa, pb, true, 0), legs, 1));
            let vi = e.input_of_cov(cov(0x93));
            set_ev(&mut e, cov(0x93), &|x| {
                if let AnyState::KobPair(p) = x {
                    p.custody = q - 1
                }
            });
            let ci = cust_in(&e, TOKEN_B, cov(0x93));
            set_tin_amount(&mut e, ci, q - 1);
            take(&mut e, TOKEN_B, 0xe5, 1);
            r.bad(&e, vi);
            let _ = ask1;
        }
    });
}

// ---------------------------------------------------------------- the merge of m = 0 next to the exit's refund

/// Swaps inputs 0 and 1 and outputs 0 and 1 (bindings follow); the order arguments name neither (the re-arm shapes).
fn swap01(e: &mut Ed) {
    e.tx.inputs.swap(0, 1);
    e.entries.swap(0, 1);
    e.plans.swap(0, 1);
    e.swap_outputs(0, 1);
    for o in e.tx.outputs.iter_mut() {
        if let Some(b) = o.covenant.as_mut() {
            b.authorizing_input = match b.authorizing_input {
                0 => 1,
                1 => 0,
                a => a,
            };
        }
    }
}

#[test]
fn ifd_merge_refund() {
    let subs = Subs::compile();
    each(&subs, &mixed(), |r, pa, pb| {
        // NIM10 a sell-first entry merges m = 0 next to its booked exit's REFUND (the exit pushes n = 0): the exit returns
        // its custody to the maker, the entry re-arms nothing. Only the entry's m > 0 refuses it (no harm done here: the
        // check keeps a merge from ever reading a refund or an update push as a traded amount).
        let mut e = ed_shape(pa, pb, "pair.rearm.ask.sellout", "NIM10 sell merge of m = 0 next to the exit's refund");
        swap01(&mut e);
        let i = ein(&e);
        let x = xin(&e);
        assert_eq!(x, 1);
        let s = ist(&e, i);
        let xs = xst(&e, x);
        let n = arg(&e, x, C_NB);
        e.tx.lock_time = 1_000 + 77_760_000;
        // the exit refunds: nb 0, its whole custody (B) back to the maker at output x with every carrier
        set_nb(&mut e, x, 0);
        let mk = maker_out(&e, TOKEN_B);
        assert_eq!(mk, x);
        set_out_amount(&mut e, mk, xs.custody);
        let xc = arg(&e, x, C_CUST) as usize;
        let want = e.entries[x].amount + e.entries[xc].amount;
        let have = e.value(mk);
        e.fund_out(mk, want as i64 - have as i64);
        // the entry merges m = 0: its custodies unchanged; the A the ask sells goes to the matcher, the matcher pays the
        // bid of B
        e.set_arg(i, NB, merge_nb(x as i64, 0));
        let ka = entry_rest(&e, TOKEN_COV).expect("A custody");
        set_out_amount(&mut e, ka, s.amount_left);
        let kb = entry_rest(&e, TOKEN_B).expect("prefund");
        set_out_amount(&mut e, kb, s.custody);
        give(&mut e, TOKEN_COV, n);
        let paid: i64 = e
            .tok_outs_of(TOKEN_B)
            .into_iter()
            .filter(|k| *k != mk && *k != kb && !e.out_state(*k).is_covenant_owned())
            .map(|k| out_amount(&e, k))
            .sum();
        take(&mut e, TOKEN_B, 161, paid + xs.custody + s.custody - xs.custody - s.custody);
        cont_state(&mut e, |c| {
            c.amount_left = s.amount_left;
            c.custody = s.custody;
        });
        r.bad(&e, i);
    });
}

// ---------------------------------------------------------------- limits: booking bound, KRON bounds, overflow

/// [`shift_front`] of the pair suite for a KobIfdPair refund: `k` matcher inputs and plain outputs in front, the entry's
/// input-index arguments (aIn, bIn, aTplIn, bTplIn) and every binding follow.
fn shift_front_refund(e: &mut Ed, k: usize) {
    use kaspa_consensus_core::tx::{TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry};
    for j in 0..k {
        let op = TransactionOutpoint::new(kaspa_consensus_core::Hash::from_bytes([0xf1; 32]), 10_000 + j as u32);
        e.tx.inputs.insert(0, TransactionInput::new_with_compute_budget(op, vec![], 0, 0));
        e.entries.insert(0, UtxoEntry::new(1_000, kob_protocol::script::p2pk_spk(&pk(MATCHER)), 500, false, None));
        e.plans.insert(0, SigPlan::P2pk { pubkey: pk(MATCHER) });
        e.tx.outputs.insert(
            0,
            TransactionOutput { value: 0, script_public_key: kob_protocol::script::p2pk_spk(&pk(MATCHER)), covenant: None },
        );
    }
    for o in e.tx.outputs.iter_mut() {
        if let Some(b) = o.covenant.as_mut() {
            b.authorizing_input += k as u16;
        }
    }
    e.tok_out = std::mem::take(&mut e.tok_out).into_iter().map(|(x, v)| (x + k, v)).collect();
    for p in e.plans.iter_mut() {
        if let SigPlan::Entry { template: TemplateId::KobIfdPair, args, .. } = p {
            for a in [AIN, BIN, ATPL, BTPL] {
                if let Arg::Int(v) = &mut args[a] {
                    *v += k as i64;
                }
            }
        }
    }
}

#[test]
fn ifd_limits() {
    let subs = Subs::compile();
    // ---- a booked exit's n must fit the merge argument: n < 2^53 (a KCC-20 A carries such amounts; scale(A) 10^9 and
    // price 1 keep the B side small). PIF12k books n = 2^53 - 1, NIF12k n = 2^53.
    each(&subs, &a_kcc(), |r, pa, pb| {
        let big = 1i64 << 53;
        let mut s = IfdPairState {
            a_scale: 1_000_000_000,
            price: 1,
            min_fill: big - 1,
            amount_left: big + 10,
            rpt_amount: big + 20,
            ..ifd_pair(MAKER_A, true, pa, pb)
        };
        let mut ex = s.exit().unwrap();
        ex.s_scale = 1_000_000_000;
        s.exit_state = IfdPairState::commit_exit(&ex);
        s.custody = s.b_custody_needed().unwrap();
        let mut b = route(vec![ifd_leg_units(s.clone(), pa, pb, big - 1, Some(NOW as i64))]);
        b.taker_tokens = vec![tutxo(pa, TOKEN_COV, 60, big + 10, pk(MATCHER), false)];
        r.ok(&edb("PIF12k booking of n = 2^53 - 1", b.clone()));
        let mut e = edb("NIF12k booking of n = 2^53", b);
        let i = ein(&e);
        set_nb(&mut e, i, big);
        let k = exit_cust(&e);
        set_out_amount(&mut e, k, big);
        edit_exit(&mut e, i, |x| {
            x.amount_left = big;
            x.custody = big;
        });
        cont_state(&mut e, |c| {
            c.amount_left -= 1;
            c.rpt_amount -= 1;
        });
        let mo = matcher_out(&e, TOKEN_COV).expect("the matcher's A change");
        e.add_out_amount(mo, -1);
        r.bad(&e, i);
    });
    each(&subs, &a_kron(), |r, pa, pb| {
        // ---- a KRON token UTXO holds at most 10^9 base units: PIL2r a buy-first entry buying 10^9 of a KRON A (the exit
        // custody holds 10^9), NIL2r 10^9 + 1 (the KRON program refuses the exit custody: fail closed)
        let kmax = 1_000_000_000i64;
        let mut s = IfdPairState { price: 1, min_fill: kmax, amount_left: kmax, ..ifd_pair(MAKER_A, true, pa, pb) };
        s.custody = s.b_custody_needed().unwrap();
        let mut b = route(vec![ifd_leg_units(s.clone(), pa, pb, kmax, None)]);
        b.taker_tokens = vec![tutxo(pa, TOKEN_COV, 60, kmax, pk(MATCHER), false)];
        r.ok(&edb("PIL2r buy-first fill of 10^9 base units of a KRON A", b.clone()));
        let mut e = edb("NIL2r buy-first fill of 10^9 + 1 base units of a KRON A", b);
        let i = ein(&e);
        let s0 = ist(&e, i);
        set_ist(&mut e, i, &IfdPairState { amount_left: kmax + 1, min_fill: kmax + 1, ..s0 });
        set_nb(&mut e, i, kmax + 1);
        let k = exit_cust(&e);
        set_out_amount(&mut e, k, kmax + 1);
        edit_exit(&mut e, i, |x| {
            x.amount_left = kmax + 1;
            x.custody = kmax + 1;
        });
        take(&mut e, TOKEN_COV, 161, 1);
        let lead = e.first_token_input(TOKEN_COV);
        r.bad(&e, lead);
        // ---- a KRON custody is authorised by a one-byte witness (its owner's input index): PIL1r the sell-first entry
        // refunding its KRON A custody from input 127, NIL1r from input 128 (fail closed: the KRON program refuses)
        for (k, ok) in [(127usize, true), (128, false)] {
            let mut e = ed_of(
                &format!("{} refund of a KRON A custody from entry input {k}", if ok { "PIL1r" } else { "NIL1r" }),
                refund_entry(ifd_pair(MAKER_A, false, pa, pb), pa, pb),
            );
            let i = ein(&e);
            shift_front_refund(&mut e, k - i);
            assert_eq!(ein(&e), k);
            if ok {
                r.ok(&e);
            } else {
                let c = cust_in(&e, TOKEN_COV, E_ID);
                r.bad(&e, c);
            }
        }
    });
    each(&subs, &mixed(), |r, pa, pb| {
        // ---- overflow: every product is checked, a quote never wraps (fail closed)
        let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIL3 the release quote overflows (price near 2^62)");
        let i = ein(&e);
        both(&mut e, i, |s| s.price = i64::MAX / 3);
        r.bad(&e, i);
        let mut e = ed_shape(pa, pb, "pair.ifd.bid.cont", "NIL4 the KAS tip product overflows");
        both(&mut e, i, |s| s.tip = i64::MAX / 3);
        r.bad(&e, i);
        let mut e = ed_shape(pa, pb, "pair.ifd.ask.cont", "NIL5 the prefund product overflows");
        let i = ein(&e);
        both(&mut e, i, |s| s.prefund = i64::MAX / 3);
        r.bad(&e, i);
    });
}
