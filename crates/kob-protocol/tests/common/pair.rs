//! Pair order fixtures (`KobPair`, `KobCondPair`, `KobIfdPair`, each one template for both sides and both families): a
//! pair A/B with A = `TOKEN_COV` (program `pa`) and B = `TOKEN_B` (program `pb`), both of scale `SCALE`, prices in B base
//! units per whole A (`RATE` = one whole B per whole A). Every shape a matcher builds for a pair order: routes through the
//! KAS books (a pair ASK's A sold to bids of A and its B bought from asks of B; a pair BID's B sold to bids of B and its A
//! bought from asks of A), netting of opposite pair orders (1 x 1, 2 x 2), evidence-armed conditional fills and updates in
//! both evidence modes, trailing, if-done fills, bookings and merges of both sides, refunds, kills, cancels.
#![allow(dead_code)]

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kob_protocol::Family;

use super::*;

/// KAS the pair orders put on each token delivery (prefunded on the order UTXO).
pub const PDC: i64 = 2 * KAS as i64;
/// A pair order's value: its own carrier plus four prefunded deliveries.
pub const PV: u64 = CARRIER + 4 * PDC as u64;
/// B base units per whole A: one whole B per whole A.
pub const RATE: i64 = WHOLE;
/// Entry and exit carriers of the pair entries.
pub const PEC: i64 = 6 * KAS as i64;

/// Token state of a fixture token of program `p`: key-held (`owner` a key) or covenant-owned custody.
pub fn tstate(p: TemplateId, amount: i64, owner: [u8; 32], covenant_owned: bool) -> TokenState {
    if covenant_owned {
        TokenState::custody(p.family(), amount, owner, ext_for(p))
    } else {
        TokenState::user(p.family(), amount, owner, ext_for(p))
    }
}

/// A token UTXO of token `token` (program `p`).
pub fn tutxo(p: TemplateId, token: [u8; 32], tag: u8, amount: i64, owner: [u8; 32], covenant_owned: bool) -> TokenUtxo {
    TokenUtxo { utxo: utxo(tag, CARRIER, 1_000, Some(token)), state: tstate(p, amount, owner, covenant_owned) }
}

fn tok_parts(p: TemplateId) -> ([u8; 32], i64, i64, i64) {
    let (h, pre, suf) = tok_fields(p);
    (h, pre, suf, p.family().code() as i64)
}

/// A plain pair order of 10 whole A at `price` B per whole A: an ASK (sells A, custody = the amount) or a BID (holds a
/// B escrow for four fills), tip 0, minimum fill one whole A.
pub fn pair(maker: u8, ask: bool, pa: TemplateId, pb: TemplateId, price: i64) -> PairState {
    let (ah, apre, asuf, afam) = tok_parts(pa);
    let (bh, bpre, bsuf, bfam) = tok_parts(pb);
    let ((sc, sh, sp, ss, sf), (tc, th, tp, ts, tf, te)) = if ask {
        ((TOKEN_COV, ah, apre, asuf, afam), (TOKEN_B, bh, bpre, bsuf, bfam, ext_for(pb)))
    } else {
        ((TOKEN_B, bh, bpre, bsuf, bfam), (TOKEN_COV, ah, apre, asuf, afam, ext_for(pa)))
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
        min_fill: WHOLE,
        price,
        tip: 0,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 0,
        amount_left: 10 * WHOLE,
        custody: 10 * WHOLE,
    };
    if !ask {
        s.custody = s.bid_escrow(s.amount_left, 4).unwrap();
    }
    s
}

/// `s` with `n` whole A left (an ask's custody follows; a bid keeps its escrow funded for four fills).
pub fn with_left(s: PairState, n: i64) -> PairState {
    let mut x = PairState { amount_left: n * WHOLE, ..s };
    x.custody = if x.is_ask() { x.amount_left } else { x.bid_escrow(x.amount_left, 4).unwrap() };
    x
}

/// The S token (custody) of a pair order: (covenant id, program).
pub fn s_of(ask: bool, pa: TemplateId, pb: TemplateId) -> ([u8; 32], TemplateId) {
    if ask {
        (TOKEN_COV, pa)
    } else {
        (TOKEN_B, pb)
    }
}

/// A resting pair order UTXO (covenant `c`) with its custody, filled for `n` whole A.
pub fn pair_leg(s: PairState, pa: TemplateId, pb: TemplateId, c: [u8; 32], tag: u8, n: i64) -> Leg {
    pair_leg_amount(s, pa, pb, c, tag, n * WHOLE)
}

/// [`pair_leg`] in base units.
pub fn pair_leg_amount(s: PairState, pa: TemplateId, pb: TemplateId, c: [u8; 32], tag: u8, amount: i64) -> Leg {
    let (tok, p) = s_of(s.is_ask(), pa, pb);
    let custody = tutxo(p, tok, tag + 1, s.custody, c, true);
    Leg::Pair { order: order(tag, PV, c, 1_000, s), custody, amount, t: None }
}

/// A plain bid of `token` (program `p`) at `price`, funded for `funded` whole tokens, filled for `n`.
pub fn kbid_leg(token: [u8; 32], p: TemplateId, maker: u8, price: i64, c: [u8; 32], tag: u8, funded: i64, n: i64) -> Leg {
    let s = BidState { token_cov_id: token, min_fill: WHOLE.min(n * WHOLE), ..bid(maker, price, p) };
    let v = s.escrow(funded * WHOLE, 1).unwrap() as u64;
    Leg::Bid { order: order(tag, v, c, 1_000, s), amount: n * WHOLE, t: None }
}

/// A plain ask of `token` (program `p`) at `price` holding `held` whole tokens, filled for `n`.
pub fn kask_leg(token: [u8; 32], p: TemplateId, maker: u8, price: i64, c: [u8; 32], tag: u8, held: i64, n: i64) -> Leg {
    let s = AskState { token_cov_id: token, amount_left: held * WHOLE, ..ask(maker, price, p) };
    Leg::Ask {
        order: order(tag, CARRIER, c, 1_000, s),
        custody: tutxo(p, token, tag + 1, held * WHOLE, c, true),
        amount: n * WHOLE,
        t: None,
    }
}

/// A matcher batch over `legs` (funded by the matcher; spread, tips and token surplus to the matcher).
pub fn route(legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: Some(pk(MATCHER)),
        taker_token_carrier: CARRIER,
        keep_surplus: vec![],
        receivers: vec![],
        payments: vec![],
        funding: vec![key_utxo(90, MATCHER, 1_000 * KAS)],
        change: Some(pk(MATCHER)),
        records: vec![],
        fee: fee(),
    }
}

pub const X_ID: [u8; 32] = [0x81; 32];
pub const Y_ID: [u8; 32] = [0x82; 32];

/// A pair ASK's route: `n` whole A sold to a bid of A, B bought from an ask of B.
pub fn route_ask(x: PairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    route(vec![
        pair_leg(x, pa, pb, X_ID, 70, n),
        kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, n),
        kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, n),
    ])
}

/// A pair BID's route: its B sold to a bid of B, `n` whole A bought from an ask of A.
pub fn route_bid(y: PairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    route(vec![
        pair_leg(y, pa, pb, Y_ID, 70, n),
        kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 72, 10, n),
        kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 74, 10, n),
    ])
}

/// `asks` pair ASKs and `bids` pair BIDs of the pair netted against each other (no KAS-book leg), every order filled for
/// one whole A per opposite order (the asks' A is what the bids receive, the bids' B what the asks receive). The bids pay
/// `RATE + 10` per whole A: the B surplus rides on the first ask's delivery.
pub fn netting(pa: TemplateId, pb: TemplateId, asks: usize, bids: usize, close: bool) -> Batch {
    let mut legs = vec![];
    let per_ask = bids as i64;
    let per_bid = asks as i64;
    for k in 0..asks {
        let x = pair(MAKER_A + k as u8, true, pa, pb, RATE);
        let x = if close { with_left(x, per_ask) } else { x };
        legs.push(pair_leg(x, pa, pb, [0x81 + k as u8; 32], 40 + 2 * k as u8, per_ask));
    }
    for k in 0..bids {
        let y = pair(20 + k as u8, false, pa, pb, RATE + 10);
        let y = if close { with_left(y, per_bid) } else { y };
        legs.push(pair_leg(y, pa, pb, [0xa1 + k as u8; 32], 100 + 2 * k as u8, per_bid));
    }
    route(legs)
}

// ---------------------------------------------------------------- conditionals

/// An OCO pair sell (side ASK): take-profit 1.20 B, stop 1.00 B with a 3% band over 300 DAA, 10 whole A.
pub fn cond_pair_ask(maker: u8, pa: TemplateId, pb: TemplateId) -> CondPairState {
    let p = pair(maker, true, pa, pb, RATE);
    CondPairState {
        maker: p.maker,
        side: SIDE_ASK,
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
        min_fill: WHOLE,
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        tp_price: RATE + 200,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: WHOLE,
        min_rest_daa: 50,
        band_daa: 300,
        keeper_tip: pktip(pa, pb),
        stop_price: RATE,
        armed: 0,
        amount_left: 10 * WHOLE,
        custody: 10 * WHOLE,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    }
}

/// An OCO pair buy (side BID): limit 0.80 B, buy stop 1.00 B (3% band over 300 DAA), 10 whole A, B escrow for its worst
/// leg plus one unit per fill.
pub fn cond_pair_bid(maker: u8, pa: TemplateId, pb: TemplateId) -> CondPairState {
    let a = cond_pair_ask(maker, pa, pb);
    let mut c = CondPairState {
        side: SIDE_BID,
        s_cov_id: a.t_cov_id,
        s_tpl_hash: a.t_tpl_hash,
        s_pre: a.t_pre,
        s_suf: a.t_suf,
        s_family: a.t_family,
        s_scale: a.t_scale,
        t_cov_id: a.s_cov_id,
        t_tpl_hash: a.s_tpl_hash,
        t_pre: a.s_pre,
        t_suf: a.s_suf,
        t_family: a.s_family,
        t_ext: ext_for(pa),
        t_scale: a.s_scale,
        tp_price: RATE - 200,
        stop_price: RATE,
        ..a
    };
    c.custody = c.bid_escrow(c.max_fills()).unwrap();
    c
}

/// A resting conditional pair order UTXO (covenant `c`) with its custody, filled for `n` whole A on `leg`.
pub fn cond_pair_leg(s: CondPairState, pa: TemplateId, pb: TemplateId, c: [u8; 32], tag: u8, n: i64, leg: u8) -> Leg {
    let (tok, p) = s_of(s.is_ask(), pa, pb);
    let custody = tutxo(p, tok, tag + 1, s.custody, c, true);
    Leg::CondPair {
        order: order(tag, PV, c, 1_000, s),
        custody,
        amount: n * WHOLE,
        leg,
        evidence: None,
        evidence_b: None,
        t: None,
        merge: None,
    }
}

/// Sets the evidence of the conditional leg at index 0.
pub fn with_evidence(mut b: Batch, ev: usize, ev_b: Option<usize>) -> Batch {
    match b.legs.first_mut() {
        Some(Leg::CondPair { evidence, evidence_b, .. }) | Some(Leg::IfdPair { evidence, evidence_b, .. }) => {
            *evidence = Some(ev);
            *evidence_b = ev_b;
        }
        _ => panic!("leg 0 is a pair conditional"),
    }
    b
}

// ---------------------------------------------------------------- if-done entries

/// The exit of a buy-first pair entry: an OCO pair sell, take-profit 1.20, stop 0.90, GTC.
pub fn ifd_pair_exit_ask(maker: u8, pa: TemplateId, pb: TemplateId) -> CondPairState {
    CondPairState { stop_price: RATE - 100, expiry_daa: NO_EXPIRY, amount_left: 0, custody: 0, ..cond_pair_ask(maker, pa, pb) }
}

/// The exit of a sell-first pair entry: an OCO pair buy-back, limit 0.80, buy stop 1.10, GTC.
pub fn ifd_pair_exit_bid(maker: u8, pa: TemplateId, pb: TemplateId) -> CondPairState {
    CondPairState { stop_price: RATE + 100, expiry_daa: NO_EXPIRY, amount_left: 0, custody: 0, ..cond_pair_bid(maker, pa, pb) }
}

/// A pair entry of 10 whole A at 1.00 B per A, minimum fill one whole A: buy-first (holds the B escrow of its whole
/// amount) or sell-first (holds A and a prefund of 0.20 B per A).
pub fn ifd_pair(maker: u8, buy: bool, pa: TemplateId, pb: TemplateId) -> IfdPairState {
    let (ah, apre, asuf, afam) = tok_parts(pa);
    let (bh, bpre, bsuf, bfam) = tok_parts(pb);
    let exit = if buy { ifd_pair_exit_ask(maker, pa, pb) } else { ifd_pair_exit_bid(maker, pa, pb) };
    let mut s = IfdPairState {
        maker: pk(maker),
        side: if buy { SIDE_BID } else { SIDE_ASK },
        a_cov_id: TOKEN_COV,
        a_tpl_hash: ah,
        a_pre: apre,
        a_suf: asuf,
        a_family: afam,
        a_scale: SCALE,
        a_ext: ext_for(pa),
        b_cov_id: TOKEN_B,
        b_tpl_hash: bh,
        b_pre: bpre,
        b_suf: bsuf,
        b_family: bfam,
        b_scale: SCALE,
        b_ext: ext_for(pb),
        price: RATE,
        prefund: if buy { 0 } else { 200 },
        tip: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: prtip(pa, pb),
        delivery_carrier: PDC,
        exit_carrier: PEC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: WHOLE,
        min_rest_daa: 50,
        keeper_tip: pktip(pa, pb),
        armed: 0,
        amount_left: 10 * WHOLE,
        custody: 0,
        rpt_amount: 0,
        exit_state: IfdPairState::commit_exit(&exit),
    };
    s.custody = s.b_custody_needed().unwrap();
    s
}

/// The KAS value of an entry UTXO.
pub fn ifd_value(s: &IfdPairState) -> u64 {
    s.kas_value().unwrap() as u64 + CARRIER
}

/// A resting pair entry UTXO (covenant `c`) with its custodies, filled for `n` whole A.
pub fn ifd_pair_leg(s: IfdPairState, pa: TemplateId, pb: TemplateId, c: [u8; 32], tag: u8, n: i64) -> Leg {
    let a_custody = (!s.is_buy_first() && s.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, tag + 1, s.amount_left, c, true));
    let b_custody = (s.custody > 0).then(|| tutxo(pb, TOKEN_B, tag + 2, s.custody, c, true));
    let v = ifd_value(&s);
    Leg::IfdPair {
        order: order(tag, v, c, 1_000, s),
        a_custody,
        b_custody,
        amount: n * WHOLE,
        evidence: None,
        evidence_b: None,
        t: None,
    }
}

pub const E_ID: [u8; 32] = [0x85; 32];
pub const XE_ID: [u8; 32] = [0x86; 32];

/// A buy-first entry's fill through the KAS books: its B sold to a bid of B, `n` whole A bought from an ask of A.
pub fn ifd_bid_route(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    route(vec![
        ifd_pair_leg(s, pa, pb, E_ID, 70, n),
        kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 74, 10, n),
        kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 76, 10, n),
    ])
}

/// A sell-first entry's fill through the KAS books: its A sold to a bid of A, B bought from an ask of B.
pub fn ifd_ask_route(s: IfdPairState, pa: TemplateId, pb: TemplateId, n: i64) -> Batch {
    route(vec![
        ifd_pair_leg(s, pa, pb, E_ID, 70, n),
        kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 74, 10, n),
        kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 76, 10, n),
    ])
}

/// The exit a repeating entry booked for `n` whole A (parent `E_ID`, rptUntil far ahead).
pub fn booked_exit(e: &IfdPairState, n: i64) -> CondPairState {
    let x_cust = if e.is_buy_first() { n * WHOLE } else { e.proceeds(n * WHOLE, e.price).unwrap() + e.pre_of(n * WHOLE).unwrap() };
    e.exit_for(n * WHOLE, x_cust, Some(Booking { parent: E_ID, until: NO_EXPIRY })).unwrap()
}

/// A booked exit's take-profit of `n` whole A (of `held` booked) re-arming its entry (which holds `entry_left` whole A
/// left, its custodies as a resting entry of that amount), through the KAS books.
pub fn rearm(buy: bool, pa: TemplateId, pb: TemplateId, held: i64, n: i64, entry_left: i64) -> Batch {
    rearm_with(buy, pa, pb, Rearm { held, n, entry_left, ..Rearm::default() })
}

/// The entry and exit of a merge shape ([`rearm_with`]).
#[derive(Clone, Copy, Debug)]
pub struct Rearm {
    /// Whole A the booked exit holds.
    pub held: i64,
    /// Whole A its take-profit fills (`n == held`: the exit sells out).
    pub n: i64,
    /// Whole A the entry has left.
    pub entry_left: i64,
    /// B base units the entry holds beyond what its amount needs (the escrow or prefund rest of an entry whose earlier
    /// fills rounded in its favour: a sold-out entry keeps it as a held B custody).
    pub b_rest: i64,
    /// The sell-first entry's prefund (B base units per whole A).
    pub prefund: i64,
    /// `Some(armed)`: a stop entry (trigger 1.00, limit 1.05 / 0.95) in that arming state; `None`: a limit entry.
    pub entry_armed: Option<i64>,
    /// The exit's `armed` (1: its stop armed by an update, the leg-0 fill then moves the band origin).
    pub exit_armed: i64,
    /// The entry's and its exit's tip.
    pub tip: i64,
}

impl Default for Rearm {
    fn default() -> Self {
        Rearm { held: 4, n: 4, entry_left: 0, b_rest: 0, prefund: 200, entry_armed: None, exit_armed: 0, tip: 0 }
    }
}

/// [`rearm`] over every merge variable: the entry's custodies (held, new, a sold-out entry's B rest), its stop and arming,
/// the exit's arming, tips.
pub fn rearm_with(buy: bool, pa: TemplateId, pb: TemplateId, r: Rearm) -> Batch {
    let Rearm { held, n, entry_left, b_rest, prefund, entry_armed, exit_armed, tip } = r;
    let mut base = ifd_pair(MAKER_A, buy, pa, pb);
    let mut exit = if buy { ifd_pair_exit_ask(MAKER_A, pa, pb) } else { ifd_pair_exit_bid(MAKER_A, pa, pb) };
    exit.tip = tip;
    base.exit_state = IfdPairState::commit_exit(&exit);
    base.tip = tip;
    if !buy {
        base.prefund = prefund;
    }
    if let Some(armed) = entry_armed {
        base.entry_stop = RATE;
        base.price = if buy { RATE + 50 } else { RATE - 50 };
        base.armed = armed;
    }
    let mut e = IfdPairState { rpt_amount: 1 + 20 * WHOLE, amount_left: entry_left * WHOLE, ..base.clone() };
    e.custody = b_rest
        + if buy {
            e.spend(e.amount_left, e.price).unwrap()
        } else if entry_left > 0 {
            e.b_custody_needed().unwrap()
        } else {
            0
        };
    let mut x = booked_exit(&base, held);
    x.armed = exit_armed;
    let (stok, sp) = s_of(x.is_ask(), pa, pb);
    let xcust = tutxo(sp, stok, 81, x.custody, XE_ID, true);
    let a_custody = (!buy && e.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, 83, e.amount_left, E_ID, true));
    let b_custody = (e.custody > 0).then(|| tutxo(pb, TOKEN_B, 84, e.custody, E_ID, true));
    let merge = PairEntryMerge { entry: order(82, ifd_value(&e), E_ID, 1_000, e), a_custody, b_custody };
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
    // every token settled exactly through the KAS books (no taker output: the shapes fit the 3 / 3 programs where they can)
    let m = n * WHOLE;
    if buy {
        // the exit sells A at its take-profit (1.20 B): A to a bid of A, its B (at least the budget plus one unit) from an
        // ask of B
        let proceeds = x.rpt_proceeds(m).unwrap();
        let t_out = x.t_out_min(m, x.tp_price).unwrap().max(proceeds + 1);
        route(vec![
            exit_leg,
            kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 74, m),
            kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 76, t_out),
        ])
    } else {
        // the exit buys A back at its limit (0.80 B): the B it pays to a bid of B, A from an ask of A
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

// ---------------------------------------------------------------- the scenarios

fn create(order_state: AnyState, value: u64, tokens: Vec<TokenUtxo>) -> Action {
    Action::CreateOrder(CreateOrder {
        order: order_state,
        value,
        tokens,
        token_carrier: CARRIER,
        funding: vec![key_utxo(2, MAKER_A, 1_000 * KAS)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: fee(),
    })
}

fn cancel(s: AnyState, value: u64, custody: Option<TokenUtxo>, prefund: Option<TokenUtxo>, strays: Vec<TokenUtxo>) -> Action {
    Action::CancelOrder(CancelOrder {
        order: order(70, value, X_ID, 1_000, s),
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

fn refund(s: AnyState, value: u64, custody: Option<TokenUtxo>, prefund: Option<TokenUtxo>, lock: u64) -> Action {
    Action::RefundOrder(RefundOrder {
        order: order(70, value, X_ID, 1_000, s),
        foreign: vec![],
        custody,
        prefund,
        lock_time: lock,
        // a funded keeper (the default tips are derived from these fees: keeper_tips.rs)
        funding: vec![key_utxo(250, KEEPER, 5 * KAS)],
        change: Some(pk(KEEPER)),
        fee: fee(),
    })
}

/// Every pair shape on the program pair (`pa` = A, `pb` = B), named `pair.*`.
pub fn pair_scenarios(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    let mut add = |name: &str, a: Action| v.push((name.to_string(), a));
    let ask = || pair(MAKER_A, true, pa, pb, RATE);
    let bid = || pair(MAKER_A, false, pa, pb, RATE);
    let maker_a = |n: i64| tutxo(pa, TOKEN_COV, 1, n * WHOLE, pk(MAKER_A), false);
    let maker_b = |n: i64| tutxo(pb, TOKEN_B, 5, n, pk(MAKER_A), false);

    // creation
    add("pair.create.ask", create(AnyState::KobPair(ask()), PV, vec![maker_a(12)]));
    add("pair.create.bid", create(AnyState::KobPair(bid()), PV, vec![maker_b(bid().custody + 7)]));
    let ca = cond_pair_ask(MAKER_A, pa, pb);
    let cb = cond_pair_bid(MAKER_A, pa, pb);
    add("pair.create.condAsk", create(AnyState::KobCondPair(ca.clone()), PV, vec![maker_a(10)]));
    add("pair.create.condBid", create(AnyState::KobCondPair(cb.clone()), PV, vec![maker_b(cb.custody)]));
    let ib = ifd_pair(MAKER_A, true, pa, pb);
    let ia = ifd_pair(MAKER_A, false, pa, pb);
    add("pair.create.ifdBid", create(AnyState::KobIfdPair(ib.clone()), ifd_value(&ib), vec![maker_b(ib.custody)]));
    add("pair.create.ifdAsk", create(AnyState::KobIfdPair(ia.clone()), ifd_value(&ia), vec![maker_a(10), maker_b(ia.custody + 3)]));

    // cancels (strays of both tokens swept), refunds, kills
    let (at, ap) = s_of(true, pa, pb);
    let (bt, bp) = s_of(false, pa, pb);
    add("pair.cancel.ask", cancel(AnyState::KobPair(ask()), PV, Some(tutxo(ap, at, 71, 10 * WHOLE, X_ID, true)), None, vec![]));
    add(
        "pair.cancel.bid.sweepStrays",
        cancel(
            AnyState::KobPair(bid()),
            PV,
            Some(tutxo(bp, bt, 71, bid().custody, X_ID, true)),
            None,
            vec![tutxo(pa, TOKEN_COV, 76, 3, X_ID, true), tutxo(pb, TOKEN_B, 77, 5, X_ID, true)],
        ),
    );
    add(
        "pair.cancel.ifdAsk",
        cancel(
            AnyState::KobIfdPair(ia.clone()),
            ifd_value(&ia),
            Some(tutxo(pa, TOKEN_COV, 71, ia.amount_left, X_ID, true)),
            Some(tutxo(pb, TOKEN_B, 72, ia.custody, X_ID, true)),
            vec![],
        ),
    );
    add("pair.refund.ask", refund(AnyState::KobPair(ask()), PV, Some(tutxo(ap, at, 71, 10 * WHOLE, X_ID, true)), None, EXPIRY as u64));
    add(
        "pair.refund.bid",
        refund(AnyState::KobPair(bid()), PV, Some(tutxo(bp, bt, 71, bid().custody, X_ID, true)), None, EXPIRY as u64),
    );
    add(
        "pair.refund.kill.ask",
        refund(
            AnyState::KobPair(PairState { tif: TIF_FOK, ..ask() }),
            PV,
            Some(tutxo(ap, at, 71, 10 * WHOLE, X_ID, true)),
            None,
            1_000 + 600,
        ),
    );
    let ia0 = IfdPairState { custody: 0, prefund: 0, ..ia.clone() };
    add(
        "pair.refund.ifdAsk.noPrefund",
        refund(
            AnyState::KobIfdPair(ia0.clone()),
            ifd_value(&ia0),
            Some(tutxo(pa, TOKEN_COV, 71, ia0.amount_left, X_ID, true)),
            None,
            EXPIRY as u64,
        ),
    );
    add(
        "pair.cancel.condBid",
        cancel(AnyState::KobCondPair(cb.clone()), PV, Some(tutxo(bp, bt, 71, cb.custody, X_ID, true)), None, vec![]),
    );
    add(
        "pair.refund.kill.bid",
        refund(
            AnyState::KobPair(PairState { tif: TIF_IOC, ..bid() }),
            PV,
            Some(tutxo(bp, bt, 71, bid().custody, X_ID, true)),
            None,
            1_000 + 600,
        ),
    );
    add(
        "pair.refund.condAsk",
        refund(AnyState::KobCondPair(ca.clone()), PV, Some(tutxo(ap, at, 71, ca.custody, X_ID, true)), None, EXPIRY as u64),
    );
    add(
        "pair.refund.condBid",
        refund(AnyState::KobCondPair(cb.clone()), PV, Some(tutxo(bp, bt, 71, cb.custody, X_ID, true)), None, EXPIRY as u64),
    );
    add(
        "pair.refund.ifdBid",
        refund(
            AnyState::KobIfdPair(ib.clone()),
            ifd_value(&ib),
            Some(tutxo(pb, TOKEN_B, 71, ib.custody, X_ID, true)),
            None,
            EXPIRY as u64,
        ),
    );
    add(
        "pair.refund.ifdAsk",
        refund(
            AnyState::KobIfdPair(ia.clone()),
            ifd_value(&ia),
            Some(tutxo(pa, TOKEN_COV, 71, ia.amount_left, X_ID, true)),
            Some(tutxo(pb, TOKEN_B, 72, ia.custody, X_ID, true)),
            EXPIRY as u64,
        ),
    );

    // KobPair through the KAS books
    add("pair.ask.rest", Action::Batch(route_ask(ask(), pa, pb, 4)));
    add("pair.ask.ioc", Action::Batch(route_ask(PairState { tif: TIF_IOC, ..ask() }, pa, pb, 4)));
    add("pair.ask.close", Action::Batch(route_ask(with_left(ask(), 4), pa, pb, 4)));
    add("pair.ask.fok", Action::Batch(route_ask(PairState { tif: TIF_FOK, ..with_left(ask(), 4) }, pa, pb, 4)));
    add("pair.bid.rest", Action::Batch(route_bid(bid(), pa, pb, 4)));
    add("pair.bid.ioc", Action::Batch(route_bid(PairState { tif: TIF_IOC, ..bid() }, pa, pb, 4)));
    add("pair.bid.done", Action::Batch(route_bid(with_left(bid(), 4), pa, pb, 4)));
    // the exact escrow: the last fill uses it up (no return)
    add("pair.bid.close", Action::Batch(route_bid(PairState { custody: 4 * WHOLE, ..with_left(bid(), 4) }, pa, pb, 4)));
    // a decaying ask (Dutch) and a rising bid, mid-way
    let dutch = PairState { price: RATE + 50, slope: 1, decay_step: 10, price_end: RATE - 50, active_from: NOW as i64 - 600, ..ask() };
    add("pair.ask.decay", Action::Batch(route_ask(dutch, pa, pb, 4)));
    let rising =
        PairState { price: RATE - 50, slope: 1, decay_step: 10, price_end: RATE + 50, active_from: NOW as i64 - 500, ..bid() };
    add(
        "pair.bid.rising",
        Action::Batch(route_bid(PairState { custody: rising.bid_escrow(10 * WHOLE, 4).unwrap(), ..rising }, pa, pb, 4)),
    );
    // TWAP with a tip
    add(
        "pair.ask.twap.tip",
        Action::Batch(route_ask(PairState { interval: 100, max_fill: 5 * WHOLE, tip: 10_000, ..ask() }, pa, pb, 4)),
    );

    // netting: 1 x 1, 2 x 2, both continuing and sold out
    add("pair.net.1x1", Action::Batch(netting(pa, pb, 1, 1, false)));
    add("pair.net.1x1.close", Action::Batch(netting(pa, pb, 1, 1, true)));
    // (two of each side need four outputs of each token: not on the 3 / 3 programs)
    let wide = pa.token_slots().unwrap().1 >= 4 && pb.token_slots().unwrap().1 >= 4;
    if wide {
        add("pair.net.2x2", Action::Batch(netting(pa, pb, 2, 2, false)));
        add("pair.net.2x2.close", Action::Batch(netting(pa, pb, 2, 2, true)));
    }

    // conditional fills: take-profit / limit, stop armed in the fill by evidence (both modes), armed stop auction
    add(
        "pair.cond.ask.tp",
        Action::Batch(route(vec![
            cond_pair_leg(ca.clone(), pa, pb, X_ID, 70, 4, 0),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 5),
        ])),
    );
    add(
        "pair.cond.bid.limit",
        Action::Batch(route(vec![
            cond_pair_leg(cb.clone(), pa, pb, X_ID, 70, 4, 0),
            kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 72, 10, 3),
            kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ])),
    );
    // sell stop armed by a resting ask of A and a resting bid of B (rate 2.50 / 2.60 < 1.00 stop: fell)
    add(
        "pair.cond.ask.stop.ev0",
        Action::Batch(with_evidence(
            route(vec![
                cond_pair_leg(ca.clone(), pa, pb, X_ID, 70, 4, 1),
                kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 5, 1),
                kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
                kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 10, 5),
                kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 10, 5),
            ]),
            1,
            Some(2),
        )),
    );
    // sell stop armed by a resting pair ASK at 0.99 (mode 1), netted against a pair BID
    add(
        "pair.cond.ask.stop.ev1",
        Action::Batch(with_evidence(
            route(vec![
                cond_pair_leg(ca.clone(), pa, pb, X_ID, 70, 4, 1),
                pair_leg(pair(MAKER_C, true, pa, pb, RATE - 10), pa, pb, cov(0x93), 72, 1),
                pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 74, 5),
            ]),
            1,
            None,
        )),
    );
    // buy stop armed by a resting bid of A and a resting ask of B (rate 2.60 / 2.50 > 1.00: rose)
    add(
        "pair.cond.bid.stop.ev0",
        Action::Batch(with_evidence(
            route(vec![
                cond_pair_leg(cb.clone(), pa, pb, X_ID, 70, 4, 1),
                kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
                kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
                kbid_leg(TOKEN_B, pb, 24, P260, cov(0x95), 76, 10, 5),
                kask_leg(TOKEN_COV, pa, 25, P250, cov(0x96), 78, 10, 5),
            ]),
            1,
            Some(2),
        )),
    );
    // buy stop armed by a resting pair BID at 1.01 (mode 1), netted against a pair ASK
    add(
        "pair.cond.bid.stop.ev1",
        Action::Batch(with_evidence(
            route(vec![
                cond_pair_leg(cb.clone(), pa, pb, X_ID, 70, 4, 1),
                pair_leg(pair(MAKER_C, false, pa, pb, RATE + 10), pa, pb, cov(0x93), 72, 1),
                pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 74, 5),
            ]),
            1,
            None,
        )),
    );
    // an armed stop's auction, sold out
    let armed = CondPairState { armed: NOW as i64 - 100, amount_left: 4 * WHOLE, custody: 4 * WHOLE, ..ca.clone() };
    add(
        "pair.cond.ask.stop.auction",
        Action::Batch(route(vec![
            cond_pair_leg(armed, pa, pb, X_ID, 70, 4, 1),
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 4),
        ])),
    );

    // updates: arm and trail without a fill, next to the evidence (both modes)
    let upd = |s: CondPairState, legs: Vec<Leg>, ev: usize, ev_b: Option<usize>| {
        let mut b = route(legs);
        b.updates.push(BatchUpdate {
            order: order(60, PV, X_ID, 1_000, AnyState::KobCondPair(s)),
            evidence: ev,
            evidence_b: ev_b,
            take: None,
        });
        Action::Batch(b)
    };
    add(
        "pair.update.ask.arm.ev0",
        upd(
            ca.clone(),
            vec![
                kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 72, 5, 1),
                kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 74, 5, 1),
                kbid_leg(TOKEN_COV, pa, 24, P260, cov(0x95), 76, 10, 1),
                kask_leg(TOKEN_B, pb, 25, P250, cov(0x96), 78, 10, 1),
            ],
            0,
            Some(1),
        ),
    );
    add(
        "pair.update.ask.arm.ev1",
        upd(
            ca.clone(),
            vec![
                pair_leg(pair(MAKER_C, true, pa, pb, RATE - 10), pa, pb, cov(0x93), 72, 1),
                pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 74, 1),
            ],
            0,
            None,
        ),
    );
    let trailing = CondPairState { trail_step: 10, trail_gap: 20, trail_wait: 0, stop_price: RATE - 300, ..ca.clone() };
    add(
        "pair.update.ask.trail.ev1",
        upd(
            trailing.clone(),
            vec![
                pair_leg(pair(MAKER_C, false, pa, pb, RATE + 10), pa, pb, cov(0x93), 72, 1),
                pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 74, 1),
            ],
            0,
            None,
        ),
    );
    add(
        "pair.update.ask.trail.ev0",
        upd(
            CondPairState { stop_price: 2 * RATE, tp_price: 4 * RATE, ..trailing.clone() },
            vec![
                kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
                kask_leg(TOKEN_B, pb, MAKER_C, P250 / 2, cov(0x94), 74, 5, 2),
                kask_leg(TOKEN_COV, pa, 24, P250, cov(0x95), 76, 10, 1),
                kbid_leg(TOKEN_B, pb, 25, P260 / 2, cov(0x96), 78, 10, 2),
            ],
            0,
            Some(1),
        ),
    );
    add(
        "pair.update.bid.trail.ev1",
        upd(
            CondPairState { trail_step: 10, trail_gap: 20, trail_wait: 0, stop_price: RATE + 300, ..cb.clone() },
            vec![
                pair_leg(pair(MAKER_C, true, pa, pb, RATE - 10), pa, pb, cov(0x93), 72, 1),
                pair_leg(pair(MAKER_B, false, pa, pb, RATE), pa, pb, cov(0x94), 74, 1),
            ],
            0,
            None,
        ),
    );
    add(
        "pair.update.bid.arm.ev0",
        upd(
            cb.clone(),
            vec![
                kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 72, 5, 1),
                kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 74, 5, 1),
                kask_leg(TOKEN_COV, pa, 24, P250, cov(0x95), 76, 10, 1),
                kbid_leg(TOKEN_B, pb, 25, P260, cov(0x96), 78, 10, 1),
            ],
            0,
            Some(1),
        ),
    );

    // if-done entries: fills through the KAS books (continuing, closing), bookings, stop entries armed in the fill
    add("pair.ifd.bid.cont", Action::Batch(ifd_bid_route(ib.clone(), pa, pb, 4)));
    let ib4 = IfdPairState { amount_left: 4 * WHOLE, ..ib.clone() };
    let ib4 = IfdPairState { custody: ib4.b_custody_needed().unwrap(), ..ib4 };
    add("pair.ifd.bid.close", Action::Batch(ifd_bid_route(ib4, pa, pb, 4)));
    add("pair.ifd.bid.book", Action::Batch(ifd_bid_route(IfdPairState { rpt_amount: 1 + 20 * WHOLE, ..ib.clone() }, pa, pb, 4)));
    add("pair.ifd.ask.cont", Action::Batch(ifd_ask_route(ia.clone(), pa, pb, 4)));
    let ia4 = IfdPairState { amount_left: 4 * WHOLE, ..ia.clone() };
    let ia4 = IfdPairState { custody: ia4.b_custody_needed().unwrap(), ..ia4 };
    add("pair.ifd.ask.close", Action::Batch(ifd_ask_route(ia4, pa, pb, 4)));
    add("pair.ifd.ask.book", Action::Batch(ifd_ask_route(IfdPairState { rpt_amount: 1 + 20 * WHOLE, ..ia.clone() }, pa, pb, 4)));
    // a buy-stop entry at 1.00 (limit 1.05) armed in its fill by a resting pair BID (mode 1)
    let sb = IfdPairState { entry_stop: RATE, price: RATE + 50, ..ib.clone() };
    let sb = IfdPairState { custody: sb.b_custody_needed().unwrap(), ..sb };
    add(
        "pair.ifd.bid.stop.ev1",
        Action::Batch(with_evidence(
            route(vec![
                ifd_pair_leg(sb, pa, pb, E_ID, 70, 4),
                pair_leg(
                    PairState { custody: RATE + 10, ..with_left(pair(MAKER_C, false, pa, pb, RATE + 10), 1) },
                    pa,
                    pb,
                    cov(0x93),
                    74,
                    1,
                ),
                kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 76, 4 * RATE + RATE + 10),
                kask_units(TOKEN_COV, pa, 24, P250, cov(0x92), 78, 5 * WHOLE),
            ]),
            1,
            None,
        )),
    );
    // a sell-stop entry at 1.00 (limit 0.95) armed in its fill by an ask of A and a bid of B (mode 0)
    let sa = IfdPairState { entry_stop: RATE, price: RATE - 50, ..ia.clone() };
    let sa = IfdPairState { custody: sa.b_custody_needed().unwrap(), ..sa };
    add(
        "pair.ifd.ask.stop.ev0",
        Action::Batch(with_evidence(
            route(vec![
                ifd_pair_leg(sa.clone(), pa, pb, E_ID, 70, 4),
                kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 74, WHOLE),
                kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 76, WHOLE),
                kbid_units(TOKEN_COV, pa, 24, P260, cov(0x95), 78, 5 * WHOLE),
                kask_units(TOKEN_B, pb, 25, P250, cov(0x96), 80, sa.proceeds(4 * WHOLE, RATE).unwrap() + WHOLE),
            ]),
            1,
            Some(2),
        )),
    );
    // stop entries armed by an update (no fill)
    let mut ub = route(vec![
        pair_leg(pair(MAKER_C, false, pa, pb, RATE + 10), pa, pb, cov(0x93), 72, 1),
        pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 74, 1),
    ]);
    let sbv = IfdPairState { entry_stop: RATE, price: RATE + 50, ..ib.clone() };
    let sbv = IfdPairState { custody: sbv.b_custody_needed().unwrap(), ..sbv };
    ub.updates.push(BatchUpdate {
        order: order(60, ifd_value(&sbv), E_ID, 1_000, AnyState::KobIfdPair(sbv)),
        evidence: 0,
        evidence_b: None,
        take: None,
    });
    add("pair.update.ifdBid.arm.ev1", Action::Batch(ub));

    // repeat merges, both sides: continuing, selling out, into an entry with nothing left (new custodies)
    add("pair.rearm.bid", Action::Batch(rearm(true, pa, pb, 4, 2, 6)));
    add("pair.rearm.bid.sellout", Action::Batch(rearm(true, pa, pb, 4, 4, 6)));
    add("pair.rearm.bid.new", Action::Batch(rearm(true, pa, pb, 4, 4, 0)));
    if wide {
        // a sell-first exit's partial re-arm keeps four outputs of B (its rest, the profit, the prefund, the B it pays)
        add("pair.rearm.ask", Action::Batch(rearm(false, pa, pb, 4, 2, 6)));
    }
    add("pair.rearm.ask.sellout", Action::Batch(rearm(false, pa, pb, 4, 4, 6)));
    add("pair.rearm.ask.new", Action::Batch(rearm(false, pa, pb, 4, 4, 0)));

    let _ = Family::Kcc20;
    v
}

/// Family pair name for test labels.
pub fn pair_name(pa: TemplateId, pb: TemplateId) -> String {
    format!("{}+{}", pa.name(), pb.name())
}

// ---------------------------------------------------------------- the branch grid (budget table)

/// A plain bid of `token` (program `p`) buying exactly `amount` base units (its minimum fill and its escrow that amount).
pub fn kbid_units(token: [u8; 32], p: TemplateId, maker: u8, price: i64, c: [u8; 32], tag: u8, amount: i64) -> Leg {
    let s = BidState { token_cov_id: token, min_fill: amount, ..bid(maker, price, p) };
    let v = s.escrow(amount, 1).unwrap() as u64;
    Leg::Bid { order: order(tag, v, c, 1_000, s), amount, t: None }
}

/// A plain ask of `token` (program `p`) holding and selling exactly `amount` base units.
pub fn kask_units(token: [u8; 32], p: TemplateId, maker: u8, price: i64, c: [u8; 32], tag: u8, amount: i64) -> Leg {
    let s = AskState { token_cov_id: token, amount_left: amount, min_fill: amount, ..ask(maker, price, p) };
    Leg::Ask { order: order(tag, CARRIER, c, 1_000, s), custody: tutxo(p, token, tag + 1, amount, c, true), amount, t: None }
}

/// The evidence legs of a pair trigger and what they release / take: `mode` "arm0" (a KAS-book order of A and one of B,
/// one whole A and `b_touch` (at least one whole) B) or "arm1" (a resting KobPair of the pair, one whole A, sold out).
/// `rate_fell`: a sell trigger (an ask of A and a bid of B, or a pair ASK at 0.99), else a buy trigger (a bid of A and an
/// ask of B, or a pair BID at 1.01). Returns (legs, A released, A taken, B released, B taken).
fn evidence_legs_b(pa: TemplateId, pb: TemplateId, mode: &str, rate_fell: bool, b_touch: i64) -> (Vec<Leg>, i64, i64, i64, i64) {
    let b = b_touch.max(WHOLE);
    match (mode, rate_fell) {
        ("arm0", true) => (
            vec![
                kask_units(TOKEN_COV, pa, MAKER_C, P250, cov(0x93), 120, WHOLE),
                kbid_units(TOKEN_B, pb, MAKER_B, P260, cov(0x94), 122, b),
            ],
            WHOLE,
            0,
            0,
            b,
        ),
        ("arm0", false) => (
            vec![
                kbid_units(TOKEN_COV, pa, MAKER_B, P260, cov(0x93), 120, WHOLE),
                kask_units(TOKEN_B, pb, MAKER_C, P250, cov(0x94), 122, b),
            ],
            0,
            WHOLE,
            b,
            0,
        ),
        ("arm1", true) => {
            let e = with_left(pair(MAKER_C, true, pa, pb, RATE - 10), 1);
            (vec![pair_leg(e, pa, pb, cov(0x93), 120, 1)], WHOLE, 0, 0, RATE - 10)
        }
        ("arm1", false) => {
            let e = PairState { custody: RATE + 10, ..with_left(pair(MAKER_C, false, pa, pb, RATE + 10), 1) };
            (vec![pair_leg(e, pa, pb, cov(0x93), 120, 1)], 0, WHOLE, RATE + 10, 0)
        }
        _ => (vec![], 0, 0, 0, 0),
    }
}

/// [`evidence_legs_b`] touching one whole B.
fn evidence_legs(pa: TemplateId, pb: TemplateId, mode: &str, rate_fell: bool) -> (Vec<Leg>, i64, i64, i64, i64) {
    evidence_legs_b(pa, pb, mode, rate_fell, WHOLE)
}

/// Settles a batch's token flows exactly through the KAS books: `a_net` > 0 base units of A released go to a bid of A,
/// < 0 come from an ask of A; the same for B.
fn settle(mut legs: Vec<Leg>, pa: TemplateId, pb: TemplateId, a_net: i64, b_net: i64) -> Vec<Leg> {
    if a_net > 0 {
        legs.push(kbid_units(TOKEN_COV, pa, 24, P260, cov(0x95), 130, a_net));
    } else if a_net < 0 {
        legs.push(kask_units(TOKEN_COV, pa, 25, P250, cov(0x96), 132, -a_net));
    }
    if b_net > 0 {
        legs.push(kbid_units(TOKEN_B, pb, 26, P260, cov(0x97), 134, b_net));
    } else if b_net < 0 {
        legs.push(kask_units(TOKEN_B, pb, 27, P250, cov(0x98), 136, -b_net));
    }
    legs
}

/// The tip of the tipped grid shapes (sompi per whole A, not a multiple of the scale: the tip's quote keeps a remainder).
pub const GRID_TIP: i64 = 10_001;

/// An update (no fill) of a conditional pair order next to its evidence, every token of the evidence settled exactly:
/// `ask` (a sell stop at 1.00, trailing from 0.70) or a buy stop (at 1.00, trailing from 1.30), arm or `trail` (step 10, or
/// 1 when `far`: a ten times larger k), evidence mode 1 (`mode1`: a resting pair order) or 0 (a KAS-book order of A and
/// one of B, touching the B the stop needs).
pub fn cond_update(pa: TemplateId, pb: TemplateId, ask: bool, trail: bool, mode1: bool, far: bool) -> Batch {
    let mut s = if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) };
    if trail {
        (s.trail_step, s.trail_gap) = (if far { 1 } else { 10 }, 20);
        s.stop_price = if ask { RATE - 300 } else { RATE + 300 };
        if !ask {
            s.custody = s.bid_escrow(s.max_fills()).unwrap();
        }
    }
    // a sell stop arms on a fall and trails up on a rise; a buy stop the other way round
    let rate_fell = ask != trail;
    let mode = if mode1 { "arm1" } else { "arm0" };
    let (legs, ra, ta, rb, tb) = evidence_legs_b(pa, pb, mode, rate_fell, s.min_touch_b().unwrap());
    let mut b = route(settle(legs, pa, pb, ra - ta, rb - tb));
    b.updates.push(BatchUpdate {
        order: order(60, PV, X_ID, 1_000, AnyState::KobCondPair(s)),
        evidence: 0,
        evidence_b: (!mode1).then_some(1),
        take: None,
    });
    b
}

/// An update arming a pair stop entry (a buy stop at 1.00 limit 1.05, or a sell stop at 1.00 limit 0.95) next to its
/// evidence (mode 1: a resting pair order; mode 0: a KAS-book order of A and one of B), settled exactly.
pub fn ifd_update(pa: TemplateId, pb: TemplateId, buy: bool, mode1: bool) -> Batch {
    let mut s = IfdPairState { entry_stop: RATE, price: if buy { RATE + 50 } else { RATE - 50 }, ..ifd_pair(MAKER_A, buy, pa, pb) };
    s.custody = s.b_custody_needed().unwrap();
    let mode = if mode1 { "arm1" } else { "arm0" };
    let (legs, ra, ta, rb, tb) = evidence_legs_b(pa, pb, mode, !buy, s.min_touch_b().unwrap());
    let mut b = route(settle(legs, pa, pb, ra - ta, rb - tb));
    b.updates.push(BatchUpdate {
        order: order(60, ifd_value(&s), E_ID, 1_000, AnyState::KobIfdPair(s)),
        evidence: 0,
        evidence_b: (!mode1).then_some(1),
        take: None,
    });
    b
}

/// Every branch of every pair input role on (`pa`, `pb`), for the compute-budget table (some do not fit the token slots
/// of the 3 / 3 programs: the generator measures the ones that build, `budget_table.rs` checks the others fail on the
/// slots only and that every role is covered). Every token flow is settled exactly (no taker output).
pub fn pair_branch_shapes(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v = pair_fill_grid(pa, pb);
    v.extend(pair_update_grid(pa, pb));
    v.extend(pair_merge_grid(pa, pb));
    v.extend(pair_refund_grid(pa, pb));
    v
}

/// The fills: the plain order (side x rest / return / close x decay x TWAP x tip), the conditional (side x leg x trigger
/// mode (arm0 / arm1 / armed auction from an origin or from an update's arming / armed whole band; the take-profit
/// unarmed or armed) x rest / return / close, unbooked or a booked exit filled without its entry, tip or not), the entry
/// (side x trigger mode x continue / close / booked / waiting, tip or not).
pub fn pair_fill_grid(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    let n = 4 * WHOLE;
    // KobPair
    for ask in [true, false] {
        for branch in ["rest", "return", "close"] {
            for decay in [false, true] {
                for (twap, tip) in [(false, 0), (false, GRID_TIP), (true, 0), (true, 10_000)] {
                    let mut s = PairState { interval: if twap { 600 } else { 0 }, tip, ..pair(MAKER_A, ask, pa, pb, RATE) };
                    if decay {
                        (s.slope, s.decay_step, s.active_from) = (1, 10, NOW as i64 - 600);
                        (s.price, s.price_end) = if ask { (RATE + 50, RATE - 50) } else { (RATE - 50, RATE + 50) };
                    }
                    if branch == "return" {
                        s.tif = TIF_IOC;
                    }
                    if branch == "close" {
                        s.amount_left = n;
                        s.custody = n;
                    }
                    if !ask {
                        s.custody = s.bid_escrow(s.amount_left, 4).unwrap();
                    }
                    let p = s.price_at(NOW as i64, 1_000).unwrap();
                    if !ask && branch == "close" {
                        s.custody = s.s_out(n, p).unwrap();
                    }
                    let s_out = s.s_out(n, p).unwrap();
                    let t_out = s.t_out_min(n, p).unwrap();
                    let legs = vec![pair_leg_amount(s, pa, pb, X_ID, 70, n)];
                    let legs = if ask { settle(legs, pa, pb, s_out, -t_out) } else { settle(legs, pa, pb, -t_out, s_out) };
                    let name = format!(
                        "pairgrid.{}.{branch}.decay{decay}{}{}",
                        if ask { "ask" } else { "bid" },
                        if twap { ".twap" } else { "" },
                        if tip > 0 { ".tip" } else { "" }
                    );
                    v.push((name, Action::Batch(route(legs))));
                }
            }
        }
    }
    // KobCondPair
    for ask in [true, false] {
        for mode in ["leg0", "leg0.armed", "arm0", "arm1", "auction", "auction.armed1", "plain"] {
            for (booked, tip) in [(false, 0), (false, GRID_TIP), (true, GRID_TIP)] {
                for branch in ["rest", "return", "close"] {
                    if ask && branch == "return" {
                        continue;
                    }
                    let mut c = if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) };
                    c.tip = tip;
                    if booked {
                        // a booked exit filled without its entry: the take-profit from rptUntil on, the stop legs
                        (c.parent, c.rpt_price, c.rpt_pre, c.rpt_until) = (E_ID, RATE, if ask { 0 } else { 200 }, NOW as i64 - 1);
                    }
                    match mode {
                        "leg0.armed" | "auction.armed1" => c.armed = 1,
                        "auction" => c.armed = NOW as i64 - 100,
                        "plain" => {
                            c.armed = 1;
                            c.band_daa = 0;
                        }
                        _ => {}
                    }
                    let leg = if mode.starts_with("leg0") { 0 } else { 1 };
                    let trigger = mode == "arm0" || mode == "arm1";
                    let lp = c.leg_price(leg, trigger, NOW as i64, 1_000).unwrap();
                    if branch != "rest" {
                        c.amount_left = n;
                        c.custody = if ask { n } else { c.bid_escrow(c.max_fills()).unwrap() };
                    }
                    let s_out = c.s_out(n, lp).unwrap();
                    let t_out = c.t_out_min(n, lp).unwrap();
                    if !ask && branch == "close" {
                        c.custody = s_out;
                    }
                    let (ev, ra, ta, rb, tb) = evidence_legs(pa, pb, mode, ask);
                    let mut legs = vec![cond_pair_leg_amount(c, pa, pb, X_ID, 70, n, leg as u8)];
                    legs.extend(ev);
                    let (a_net, b_net) = if ask { (n + ra - ta, rb - tb - t_out) } else { (ra - ta - t_out, s_out + rb - tb) };
                    let mut b = route(settle(legs, pa, pb, a_net, b_net));
                    if trigger {
                        b = with_evidence(b, 1, (mode == "arm0").then_some(2));
                    }
                    let name = format!(
                        "pairgrid.cond.{}.{mode}.{branch}{}{}",
                        if ask { "ask" } else { "bid" },
                        if booked { ".booked" } else { "" },
                        if tip > 0 { ".tip" } else { "" }
                    );
                    v.push((name, Action::Batch(b)));
                }
            }
        }
    }
    // KobIfdPair
    for buy in [true, false] {
        for mode in ["limit", "arm0", "arm1", "auction", "auction.armed1"] {
            for tip in [0, GRID_TIP] {
                for kind in ["cont", "close", "book.cont", "book.wait", "wait"] {
                    let mut s = ifd_pair(MAKER_A, buy, pa, pb);
                    s.tip = tip;
                    if mode != "limit" {
                        s.entry_stop = RATE;
                        s.price = if buy { RATE + 50 } else { RATE - 50 };
                    }
                    match mode {
                        "auction" => s.armed = NOW as i64 - 150,
                        "auction.armed1" => s.armed = 1,
                        _ => {}
                    }
                    match kind {
                        "close" => s.amount_left = n,
                        "book.cont" => s.rpt_amount = 1 + 20 * WHOLE,
                        "book.wait" => {
                            s.amount_left = n;
                            s.rpt_amount = 1 + 20 * WHOLE;
                        }
                        "wait" => {
                            s.amount_left = n;
                            s.rpt_amount = 1 + 2 * WHOLE;
                        }
                        _ => {}
                    }
                    s.custody = s.b_custody_needed().unwrap();
                    let trigger = mode == "arm0" || mode == "arm1";
                    let p = s.price_at(trigger, NOW as i64, 1_000).unwrap();
                    let (ev, ra, ta, rb, tb) = evidence_legs(pa, pb, mode, !buy);
                    let mut legs = vec![ifd_pair_leg(s.clone(), pa, pb, E_ID, 70, 4)];
                    legs.extend(ev);
                    let (a_net, b_net) = if buy {
                        (ra - ta - n, s.spend(n, p).unwrap() + rb - tb)
                    } else {
                        (n + ra - ta, rb - tb - s.proceeds(n, p).unwrap())
                    };
                    let mut b = route(settle(legs, pa, pb, a_net, b_net));
                    if trigger {
                        b = with_evidence(b, 1, (mode == "arm0").then_some(2));
                    }
                    let name =
                        format!("pairgrid.ifd.{}.{mode}.{kind}{}", if buy { "bid" } else { "ask" }, if tip > 0 { ".tip" } else { "" });
                    v.push((name, Action::Batch(b)));
                }
            }
        }
    }
    v
}

/// The updates: the conditional's arm and trail (near and far) of both sides in both evidence modes, the stop entry's arm
/// of both sides in both evidence modes.
pub fn pair_update_grid(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    for ask in [true, false] {
        let side = if ask { "ask" } else { "bid" };
        for mode1 in [false, true] {
            let ev = if mode1 { "ev1" } else { "ev0" };
            v.push((format!("pairgrid.update.{side}.arm.{ev}"), Action::Batch(cond_update(pa, pb, ask, false, mode1, false))));
            for far in [false, true] {
                v.push((
                    format!("pairgrid.update.{side}.trail.{ev}{}", if far { ".far" } else { "" }),
                    Action::Batch(cond_update(pa, pb, ask, true, mode1, far)),
                ));
            }
            // the entry's side: a buy-first entry is a BID, a sell-first one an ASK
            v.push((format!("pairgrid.update.ifd.{side}.arm.{ev}"), Action::Batch(ifd_update(pa, pb, !ask, mode1))));
        }
    }
    v
}

/// The merges: side x the entry's custodies (held with whole A left, a sold-out entry holding only its B rest, nothing
/// held: new custodies) x partial / sell-out x the entry's stop and arming (where it has A left) x the exit's arming, with
/// tips.
pub fn pair_merge_grid(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    for buy in [true, false] {
        for (label, entry_left, b_rest) in [("left", 6, 0), ("rest", 0, 7), ("new", 0, 0)] {
            for n in [2, 4] {
                let armings: &[Option<i64>] =
                    if entry_left > 0 { &[None, Some(0), Some(1), Some(NOW as i64 - 150)] } else { &[None, Some(1)] };
                for entry_armed in armings.iter().copied() {
                    for exit_armed in [0, 1] {
                        let r = Rearm { held: 4, n, entry_left, b_rest, entry_armed, exit_armed, tip: GRID_TIP, ..Rearm::default() };
                        let name = format!(
                            "pairgrid.rearm.{}.{label}.n{n}.entry{}.exit{exit_armed}",
                            if buy { "bid" } else { "ask" },
                            match entry_armed {
                                None => "Limit".to_string(),
                                Some(a) if a > 1 => "Origin".to_string(),
                                Some(a) => format!("Armed{a}"),
                            }
                        );
                        v.push((name, Action::Batch(rearm_with(buy, pa, pb, r))));
                    }
                }
            }
        }
    }
    v
}

/// The refunds of every custody state (at their due DAA): the plain order (side x GTC / IOC / FOK), the conditional (side
/// x unbooked / a booked exit x unarmed / armed), the entry (buy-first: its escrow, a waiting entry's escrow rest only,
/// nothing held; sell-first: A and its prefund, A only, a waiting entry's prefund rest only, nothing held).
pub fn pair_refund_grid(pa: TemplateId, pb: TemplateId) -> Vec<(String, Action)> {
    let mut v: Vec<(String, Action)> = vec![];
    let due = |s: &AnyState| s.refund_due(1_000).unwrap() as u64;
    for ask in [true, false] {
        let side = if ask { "ask" } else { "bid" };
        let (tok, p) = s_of(ask, pa, pb);
        for (tif, name) in [(0, "gtc"), (TIF_IOC, "ioc"), (TIF_FOK, "fok")] {
            let s = PairState { tif, ..with_left(pair(MAKER_A, ask, pa, pb, RATE), 3) };
            let c = tutxo(p, tok, 71, s.custody, X_ID, true);
            let st = AnyState::KobPair(s);
            v.push((format!("pairgrid.refund.pair.{side}.{name}"), refund(st.clone(), PV, Some(c), None, due(&st))));
        }
        for booked in [false, true] {
            for armed in [0, 1] {
                let mut s = if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) };
                s.armed = armed;
                if booked {
                    (s.parent, s.rpt_price, s.rpt_pre, s.rpt_until) = (E_ID, RATE, if ask { 0 } else { 200 }, NO_EXPIRY);
                    s.expiry_daa = NO_EXPIRY;
                }
                let c = tutxo(p, tok, 71, s.custody, X_ID, true);
                let st = AnyState::KobCondPair(s);
                let name = format!("pairgrid.refund.cond.{side}{}.armed{armed}", if booked { ".booked" } else { "" });
                v.push((name, refund(st.clone(), PV, Some(c), None, due(&st))));
            }
        }
    }
    // the entry: (label, buy-first, amount left, B held, prefund)
    let entries = [
        ("bid.escrow", true, 10, None, 0),
        ("bid.rest", true, 0, Some(7), 0),
        ("bid.nothing", true, 0, Some(0), 0),
        ("ask.two", false, 10, None, 200),
        ("ask.aOnly", false, 10, Some(0), 0),
        ("ask.prefundRest", false, 0, Some(7), 200),
        ("ask.nothing", false, 0, Some(0), 200),
    ];
    for (label, buy, left, b_held, prefund) in entries {
        let mut s = ifd_pair(MAKER_A, buy, pa, pb);
        s.prefund = prefund;
        s.amount_left = left * WHOLE;
        if left == 0 {
            // a repeating entry waiting for its exits
            s.rpt_amount = 1 + 20 * WHOLE;
        }
        s.custody = b_held.unwrap_or_else(|| s.b_custody_needed().unwrap());
        let a = (!buy && s.amount_left > 0).then(|| tutxo(pa, TOKEN_COV, 71, s.amount_left, X_ID, true));
        let b = (s.custody > 0).then(|| tutxo(pb, TOKEN_B, 72, s.custody, X_ID, true));
        // the custodies in the order the entry holds them: A first, then B
        let (first, second) = if a.is_some() { (a, b) } else { (b, None) };
        let value = ifd_value(&s);
        let st = AnyState::KobIfdPair(s);
        v.push((format!("pairgrid.refund.ifd.{label}"), refund(st.clone(), value, first, second, due(&st))));
    }
    v
}

/// [`cond_pair_leg`] filled for `amount` base units.
pub fn cond_pair_leg_amount(s: CondPairState, pa: TemplateId, pb: TemplateId, c: [u8; 32], tag: u8, amount: i64, leg: u8) -> Leg {
    let (tok, p) = s_of(s.is_ask(), pa, pb);
    let custody = tutxo(p, tok, tag + 1, s.custody, c, true);
    Leg::CondPair { order: order(tag, PV, c, 1_000, s), custody, amount, leg, evidence: None, evidence_b: None, t: None, merge: None }
}
