//! Pair order fixtures for the matcher tests: pair orders of token A (program `pa`) and token B (program `pb`) — `KobPair`
//! asks and bids, `KobCondPair` conditionals, `KobIfdPair` entries — with their exact custodies, plain KAS orders of both
//! tokens, and the checks every transaction that fills pair orders must pass. Prices are base units of B per whole A
//! (`RATE` = one whole B per whole A); both tokens have the scale `SCALE`.
#![allow(dead_code)]

use std::collections::BTreeSet;

use super::*;
use kob_executor::matcher::engine::{tick, TickInput, TickReport};

/// Token B of the KCC-20 programs, and a second KRON token (when A is KRON too).
pub const TOKEN_B: [u8; 32] = [0x72; 32];
pub const TOKEN_B_KRON: [u8; 32] = [0x73; 32];
/// KAS a pair order puts on each token delivery, and its value (carrier plus four deliveries).
pub const PDC: i64 = 2 * KAS as i64;
pub const PV: u64 = CARRIER + 4 * PDC as u64;
/// Entry and exit carriers of the pair entries.
pub const PEC: i64 = 6 * KAS as i64;
/// One whole B per whole A.
pub const RATE: i64 = WHOLE;
/// KAS prices of the KAS books used by most scenarios.
pub const P200: i64 = 200_000_000;
/// A pair order's KAS tip that pays a netting transaction's fee (sompi per whole A: 0.05 KAS).
pub const PTIP: i64 = 5_000_000;

/// The default refund tip and keeper tip of a pair order of programs `pa` / `pb` (the pair table: a pair order's keeper
/// transaction carries both tokens' programs, a sell-first entry's refund returns two custodies).
pub fn prtip(pa: TemplateId, pb: TemplateId) -> i64 {
    kob_protocol::defaults::pair_tips(pa, pb).expect("pair tips").refund_tip as i64
}
pub fn pktip(pa: TemplateId, pb: TemplateId) -> i64 {
    kob_protocol::defaults::pair_tips(pa, pb).expect("pair tips").keeper_tip as i64
}

pub fn fam(p: TemplateId) -> Family {
    p.family()
}

/// Token A's covenant id for program `pa`.
pub fn token_a(pa: TemplateId) -> [u8; 32] {
    if fam(pa) == Family::Kron {
        TOKEN_COV_KRON
    } else {
        TOKEN
    }
}

/// Token B's covenant id for program `pb` (never token A).
pub fn token_b(pa: TemplateId, pb: TemplateId) -> [u8; 32] {
    match (fam(pa), fam(pb)) {
        (_, Family::Kcc20) => TOKEN_B,
        (Family::Kron, Family::Kron) => TOKEN_B_KRON,
        (Family::Kcc20, Family::Kron) => TOKEN_COV_KRON,
    }
}

pub fn ext_of(p: TemplateId) -> [u8; 32] {
    if fam(p) == Family::Kron {
        [0; 32]
    } else {
        EXT
    }
}

/// A token state of program `p`: covenant-owned (custody) or key-owned.
pub fn tstate(p: TemplateId, amount: i64, owner: [u8; 32], covenant_owned: bool) -> TokenState {
    match (fam(p), covenant_owned) {
        (Family::Kron, true) => KronState::custody(amount, owner).into(),
        (Family::Kron, false) => KronState::addr(amount, owner).into(),
        (Family::Kcc20, true) => Kcc20State::custody(amount, owner, EXT).into(),
        (Family::Kcc20, false) => Kcc20State::p2pk(amount, owner, EXT).into(),
    }
}

/// A custody UTXO of `token` (program `p`) owned by order `owner`.
pub fn tcustody(p: TemplateId, token: [u8; 32], amount: i64, owner: [u8; 32], daa: u64) -> TokenUtxo {
    TokenUtxo { utxo: utxo(CARRIER, daa, Some(token)), state: tstate(p, amount, owner, true) }
}

fn tok_parts(p: TemplateId) -> ([u8; 32], i64, i64, i64) {
    let (h, pre, suf) = tok_fields(p);
    (h, pre, suf, fam(p).code() as i64)
}

/// A plain pair order of `amount` base units of A at `price` B per whole A with KAS tip `tip` per whole A: an ASK (sells A,
/// custody = the amount) or a BID (holds a B escrow for four fills); minimum fill one whole A (or the amount).
#[allow(clippy::too_many_arguments)]
pub fn pair_state(maker: u8, ask: bool, pa: TemplateId, pb: TemplateId, amount: i64, price: i64, tip: i64, tif: i64) -> PairState {
    let (ah, apre, asuf, afam) = tok_parts(pa);
    let (bh, bpre, bsuf, bfam) = tok_parts(pb);
    let (ta, tb) = (token_a(pa), token_b(pa, pb));
    let ((sc, sh, sp, ss, sf), (tc, th, tp, ts, tf, te)) = if ask {
        ((ta, ah, apre, asuf, afam), (tb, bh, bpre, bsuf, bfam, ext_of(pb)))
    } else {
        ((tb, bh, bpre, bsuf, bfam), (ta, ah, apre, asuf, afam, ext_of(pa)))
    };
    let mut s = PairState {
        maker: pk(maker),
        side: if ask { SIDE_ASK } else { SIDE_BID },
        s_cov_id: sc,
        s_tpl_hash: sh,
        s_pre: sp,
        s_suf: ss,
        s_family: sf,
        s_scale: SCALE,
        t_cov_id: tc,
        t_tpl_hash: th,
        t_pre: tp,
        t_suf: ts,
        t_family: tf,
        t_ext: te,
        t_scale: SCALE,
        min_fill: WHOLE.min(amount),
        price,
        tip,
        tif,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: amount,
        custody: amount,
    };
    if !ask {
        s.custody = s.bid_escrow(s.amount_left, 4).expect("escrow");
    }
    s
}

pub fn pask(maker: u8, pa: TemplateId, pb: TemplateId, amount: i64, price: i64, tip: i64) -> PairState {
    pair_state(maker, true, pa, pb, amount, price, tip, TIF_GTC)
}

pub fn pbid(maker: u8, pa: TemplateId, pb: TemplateId, amount: i64, price: i64, tip: i64) -> PairState {
    pair_state(maker, false, pa, pb, amount, price, tip, TIF_GTC)
}

/// The program of the token a pair order holds (S).
fn s_prog(ask: bool, pa: TemplateId, pb: TemplateId) -> TemplateId {
    if ask {
        pa
    } else {
        pb
    }
}

/// A listed pair order with its exact custody of S.
pub fn l_pair(id: u32, pa: TemplateId, pb: TemplateId, s: PairState, daa: u64) -> ListedOrder {
    let custody = tcustody(s_prog(s.is_ask(), pa, pb), s.s_cov_id, s.custody, cid(id), daa);
    let v = PV + s.tip_kas(s.amount_left).expect("tip") as u64;
    ListedOrder {
        family: fam(pa),
        order: OrderUtxo { utxo: utxo(v, daa, Some(cid(id))), state: AnyState::KobPair(s) },
        custody: Some(custody),
        custody_b: None,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    }
}

/// An OCO pair conditional of `amount` base units of A: a sell (ASK, stop below the market, take-profit above) or a buy
/// (BID, buy stop above, limit below), 3% band over 300 DAA, minimum touch one whole A, 50 DAA rest; `tp` / `stop` 0 for
/// none.
#[allow(clippy::too_many_arguments)]
pub fn cond_pair(maker: u8, ask: bool, pa: TemplateId, pb: TemplateId, amount: i64, tp: i64, stop: i64) -> CondPairState {
    let p = pair_state(maker, ask, pa, pb, amount, RATE, 0, TIF_GTC);
    let mut c = CondPairState {
        maker: p.maker,
        side: p.side,
        s_cov_id: p.s_cov_id,
        s_tpl_hash: p.s_tpl_hash,
        s_pre: p.s_pre,
        s_suf: p.s_suf,
        s_family: p.s_family,
        s_scale: p.s_scale,
        t_cov_id: p.t_cov_id,
        t_tpl_hash: p.t_tpl_hash,
        t_pre: p.t_pre,
        t_suf: p.t_suf,
        t_family: p.t_family,
        t_ext: p.t_ext,
        t_scale: p.t_scale,
        min_fill: WHOLE.min(amount),
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        tp_price: tp,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: WHOLE,
        min_rest_daa: 50,
        band_daa: 300,
        keeper_tip: pktip(pa, pb),
        stop_price: stop,
        armed: 0,
        amount_left: amount,
        custody: amount,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    };
    if !ask {
        c.custody = c.bid_escrow(c.max_fills()).expect("escrow");
    }
    c
}

/// A listed pair conditional with its exact custody of S.
pub fn l_cond_pair(id: u32, pa: TemplateId, pb: TemplateId, s: CondPairState, daa: u64) -> ListedOrder {
    let custody = tcustody(s_prog(s.is_ask(), pa, pb), s.s_cov_id, s.custody, cid(id), daa);
    let v = PV + s.tip_kas(s.amount_left).expect("tip") as u64;
    ListedOrder {
        family: fam(pa),
        order: OrderUtxo { utxo: utxo(v, daa, Some(cid(id))), state: AnyState::KobCondPair(s) },
        custody: Some(custody),
        custody_b: None,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    }
}

/// The exit a pair entry commits: buy-first an OCO sell (take-profit `tp`, stop `stop`), sell-first an OCO buy-back.
pub fn pair_exit(maker: u8, buy_first: bool, pa: TemplateId, pb: TemplateId, tp: i64, stop: i64) -> CondPairState {
    CondPairState { expiry_daa: NO_EXPIRY, amount_left: 0, custody: 0, ..cond_pair(maker, buy_first, pa, pb, WHOLE, tp, stop) }
}

/// A pair entry of `amount` base units of A at `price` B per whole A (minimum fill one whole A): buy-first (holds the B
/// escrow of its whole amount; exit: sell at take-profit `price + 200`, stop `price - 100`) or sell-first (holds A and a
/// prefund of 0.20 B per A; exit: buy back at `price - 200`, stop `price + 100`).
pub fn ifd_pair(maker: u8, buy: bool, pa: TemplateId, pb: TemplateId, amount: i64, price: i64) -> IfdPairState {
    let (ah, apre, asuf, afam) = tok_parts(pa);
    let (bh, bpre, bsuf, bfam) = tok_parts(pb);
    let exit = if buy {
        pair_exit(maker, true, pa, pb, price + 200, price - 100)
    } else {
        pair_exit(maker, false, pa, pb, price - 200, price + 100)
    };
    let mut s = IfdPairState {
        maker: pk(maker),
        side: if buy { SIDE_BID } else { SIDE_ASK },
        a_cov_id: token_a(pa),
        a_tpl_hash: ah,
        a_pre: apre,
        a_suf: asuf,
        a_family: afam,
        a_scale: SCALE,
        a_ext: ext_of(pa),
        b_cov_id: token_b(pa, pb),
        b_tpl_hash: bh,
        b_pre: bpre,
        b_suf: bsuf,
        b_family: bfam,
        b_scale: SCALE,
        b_ext: ext_of(pb),
        price,
        prefund: if buy { 0 } else { 200 },
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        exit_carrier: PEC,
        min_fill: WHOLE.min(amount),
        entry_stop: 0,
        band_daa: 300,
        min_touch: WHOLE,
        min_rest_daa: 50,
        keeper_tip: pktip(pa, pb),
        armed: 0,
        amount_left: amount,
        custody: 0,
        rpt_amount: 0,
        exit_state: IfdPairState::commit_exit(&exit),
    };
    s.custody = s.b_custody_needed().expect("custody");
    s
}

/// A listed pair entry with its custodies (sell-first: A then the B prefund; buy-first: the B escrow).
pub fn l_ifd_pair(id: u32, pa: TemplateId, pb: TemplateId, s: IfdPairState, daa: u64) -> ListedOrder {
    let a = (!s.is_buy_first() && s.amount_left > 0).then(|| tcustody(pa, s.a_cov_id, s.amount_left, cid(id), daa));
    let b = (s.custody > 0).then(|| tcustody(pb, s.b_cov_id, s.custody, cid(id), daa));
    let (custody, custody_b) = match (a, b) {
        (Some(a), b) => (Some(a), b),
        (None, b) => (b, None),
    };
    let v = s.kas_value().expect("kas") as u64 + CARRIER;
    ListedOrder {
        family: fam(pa),
        order: OrderUtxo { utxo: utxo(v, daa, Some(cid(id))), state: AnyState::KobIfdPair(s) },
        custody,
        custody_b,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    }
}

/// A plain KAS bid of `token` (program `p`) at `price`, funded for `amount` base units.
pub fn l_bid_of(id: u32, token: [u8; 32], p: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    let mut b = bid(maker, price, p);
    b.token_cov_id = token;
    b.extension_commitment = ext_of(p);
    b.refund_tip = rtip(p);
    let v = (b.used(amount).expect("budget") + b.delivery_carrier + b.reserve) as u64;
    ListedOrder {
        family: fam(p),
        order: OrderUtxo { utxo: utxo(v, 1_000, Some(cid(id))), state: AnyState::KobBid(b).into_family(fam(p)) },
        custody: None,
        custody_b: None,
        deadline: None,
        seen_daa: 1_000,
        foreign: vec![],
        strays: vec![],
    }
}

/// A plain KAS ask of `token` (program `p`) at `price` holding `amount` base units.
pub fn l_ask_of(id: u32, token: [u8; 32], p: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    let mut a = ask(maker, price, amount, p);
    a.token_cov_id = token;
    a.refund_tip = rtip(p);
    let custody = tcustody(p, token, a.custody_amount(), cid(id), 1_000);
    ListedOrder {
        family: fam(p),
        order: OrderUtxo { utxo: utxo(CARRIER, 1_000, Some(cid(id))), state: AnyState::KobAsk(a).into_family(fam(p)) },
        custody: Some(custody),
        custody_b: None,
        deadline: None,
        seen_daa: 1_000,
        foreign: vec![],
        strays: vec![],
    }
}

/// KAS bids / asks of token A and B (programs `pa`, `pb`).
pub fn l_bid_a(id: u32, pa: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    l_bid_of(id, token_a(pa), pa, maker, price, amount)
}
pub fn l_ask_a(id: u32, pa: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    l_ask_of(id, token_a(pa), pa, maker, price, amount)
}
pub fn l_bid_b(id: u32, pa: TemplateId, pb: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    l_bid_of(id, token_b(pa, pb), pb, maker, price, amount)
}
pub fn l_ask_b(id: u32, pa: TemplateId, pb: TemplateId, maker: u8, price: i64, amount: i64) -> ListedOrder {
    l_ask_of(id, token_b(pa, pb), pb, maker, price, amount)
}

/// KAS the operator's token outputs would carry in the pair tests (distinctive: no order uses it), so that a token output
/// left to the operator is recognised.
pub const OP_TOKEN_CARRIER: u64 = 777_777_777;

/// Runs a tick over `orders` (no KRON twin: a pair names two tokens); no anomaly, every pair fill checked.
pub fn run_pair(orders: Vec<ListedOrder>, cfg: &EngineConfig) -> TickReport {
    let b = book(orders);
    let inp = input(&b);
    let cfg = EngineConfig { token_carrier: OP_TOKEN_CARRIER, ..cfg.clone() };
    let r = tick(&inp, &cfg, &Families::default(), &signer());
    assert_eq!(r.slack_retries, 0, "a compute budget of the table was one unit short");
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
    assert!(r.quarantined.is_empty(), "quarantined: {:?}", r.quarantined);
    check_pair(&r, &inp);
    r
}

/// Base units of order `id` over the report's transactions.
pub fn amount_in(r: &TickReport, id: [u8; 32]) -> i64 {
    r.prepared.iter().map(|p| p.plan.amount_of(&id)).sum()
}

/// Every prepared transaction was engine-validated (the covenants enforce every pair order's own guarantees), is
/// profitable, spends each order once, leaves the operator no token (no inventory: no token output carries the operator's
/// token carrier) and fills each pair order within its quantity rules, in one transaction of the tick.
pub fn check_pair(r: &TickReport, inp: &TickInput) {
    let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
    for p in &r.prepared {
        assert!(p.validation.is_some(), "unvalidated transaction");
        assert!(p.accounting.profit >= 1, "an unprofitable batch: {}", p.accounting.profit);
        let filled: BTreeSet<[u8; 32]> = p.plan.fills.iter().map(|f| f.cand.id).collect();
        assert_eq!(filled.len(), p.plan.fills.len(), "an order twice in one batch");
        for (i, o) in p.signed.tx.outputs.iter().enumerate() {
            assert!(!(o.covenant.is_some() && o.value == OP_TOKEN_CARRIER), "output {i} leaves tokens to the operator");
        }
        check_triggers(p, inp);
        let b = batch_request(p);
        assert!(b.taker_tokens.is_empty(), "the reference planner trades no inventory");
        for f in p.plan.fills.iter().filter(|f| f.cand.pair.is_some()) {
            assert!(seen.insert(f.cand.id), "a pair order filled by two transactions of one tick");
            let o = inp.orders.iter().find(|o| o.id() == f.cand.id).expect("listed");
            let x = &f.cand.pair.as_ref().expect("pair").info;
            assert!(x.quantity_ok(f.amount), "a pair fill of {} breaks its quantity rules", f.amount);
            if let Some(l) = o.order.state.amount_left() {
                assert!(f.amount <= l);
            }
        }
    }
}

/// A listed pair order moved onto tokens `a` / `b` (state, exit commitment and custodies): books of many tokens name their
/// tokens freely, the fixtures build every pair order on `token_a` / `token_b`.
pub fn pair_retoken(mut o: ListedOrder, a: [u8; 32], b: [u8; 32]) -> ListedOrder {
    match &mut o.order.state {
        AnyState::KobPair(s) => {
            let ask = s.is_ask();
            (s.s_cov_id, s.t_cov_id) = if ask { (a, b) } else { (b, a) };
        }
        AnyState::KobCondPair(s) => {
            let ask = s.is_ask();
            (s.s_cov_id, s.t_cov_id) = if ask { (a, b) } else { (b, a) };
        }
        AnyState::KobIfdPair(s) => {
            (s.a_cov_id, s.b_cov_id) = (a, b);
            let mut e = s.exit().expect("exit");
            let ask = e.is_ask();
            (e.s_cov_id, e.t_cov_id) = if ask { (a, b) } else { (b, a) };
            s.exit_state = IfdPairState::commit_exit(&e);
        }
        _ => {}
    }
    let sell_first_entry = matches!(&o.order.state, AnyState::KobIfdPair(s) if !s.is_buy_first());
    let held = match &o.order.state {
        AnyState::KobPair(s) => s.s_cov_id,
        AnyState::KobCondPair(s) => s.s_cov_id,
        _ => b,
    };
    if let Some(c) = &mut o.custody {
        c.utxo.covenant_id = Some(if sell_first_entry { a } else { held });
    }
    if let Some(c) = &mut o.custody_b {
        c.utxo.covenant_id = Some(b);
    }
    o
}

/// Delta-debugging of a failing view: the smallest sub-list of `orders` (a 1-minimal one) for which `fails` still holds, to
/// turn a fuzzer's anomaly into a minimal reproduction.
pub fn ddmin_orders(orders: &[ListedOrder], fails: &dyn Fn(&[ListedOrder]) -> bool) -> Vec<ListedOrder> {
    let mut cur = orders.to_vec();
    let mut n = 2usize;
    while cur.len() >= 2 {
        let parts = n;
        let chunk = cur.len().div_ceil(parts);
        let mut reduced = false;
        for i in 0..parts {
            let (s, e) = (i * chunk, ((i + 1) * chunk).min(cur.len()));
            if s >= e {
                continue;
            }
            let rest: Vec<ListedOrder> = cur[..s].iter().chain(cur[e..].iter()).cloned().collect();
            if !rest.is_empty() && fails(&rest) {
                cur = rest;
                n = n.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
        }
        if !reduced {
            if n >= cur.len() {
                break;
            }
            n = (n * 2).min(cur.len());
        }
    }
    cur
}

/// One line per order of a view: kind, id, state (JSON), custodies.
pub fn describe_orders(orders: &[ListedOrder]) -> String {
    orders
        .iter()
        .map(|o| {
            format!(
                "{} #{} daa {} value {} {} custody {:?} custody_b {:?}",
                o.order.state.template_id().name(),
                u32::from_le_bytes(o.id()[..4].try_into().unwrap()),
                o.order.utxo.block_daa_score,
                o.order.utxo.amount,
                serde_json::to_string(&o.order.state).unwrap_or_default(),
                o.custody.as_ref().map(|c| (c.utxo.covenant_id.map(|x| x[0]), c.utxo.block_daa_score)),
                o.custody_b.as_ref().map(|c| c.utxo.covenant_id.map(|x| x[0])),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
