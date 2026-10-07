//! Deterministic fixtures: one request per builder shape (the golden-vector set), plus the token
//! slot grid used to measure the compute-budget table. Keys are fixed (`[n; 32]`), UTXO ids are
//! synthetic, covenant ids of existing orders are arbitrary (the engine checks the covenant
//! context of the spending transaction, not the history of its inputs). Tips are the per-program
//! defaults of `kob_protocol::defaults` (derived from measured fees).
//!
//! **KRON.** The fixtures are written once, for KCC-20 states, and take the token program as a
//! parameter. For a KRON program the state-dependent parts follow the program (token template
//! fields, no extension commitment, exit commitment length) and [`kron_action`] converts what does
//! not carry the program: order-kind tags (`KobAsk` -> `KobAskKron`) and token UTXO states (owner
//! scheme 0 -> `id_type` 3, 4 -> 2, extension dropped). It also
//! adds the P2PK input every address-presence token input needs (KRON authorises a key-held token
//! by a P2PK input of that key in the transaction).
#![allow(dead_code)]

pub mod branches;
pub mod exact;
pub mod pair;

use std::collections::BTreeMap;

use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::defaults::tips;
use kob_protocol::payload::Record;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kob_protocol::Family;

pub const KAS: u64 = 100_000_000;
pub const CARRIER: u64 = 10 * KAS;
pub const DC: i64 = 10 * KAS as i64;
pub const EC: i64 = 10 * KAS as i64;

/// The carriers the scenarios are built with. [`Carriers::FIXTURE`] (the constants above and `pair::PDC` / `pair::PEC`)
/// unless a test runs them under [`with_carriers`]: the order side (order UTXOs, custodies, the maker's token inputs and
/// sends, deliveries, exits) and the matcher's taker token carrier separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Carriers {
    /// Order UTXOs, custodies, key-held token UTXOs and token sends.
    pub order: u64,
    /// `deliveryCarrier` / `exitCarrier` of the KAS-quoted kinds.
    pub delivery: i64,
    pub exit: i64,
    /// `deliveryCarrier` / `exitCarrier` of the pair kinds.
    pub pair_delivery: i64,
    pub pair_exit: i64,
    /// The matcher's (or an x402 payer's) token output carrier, `Batch::taker_token_carrier`.
    pub taker: u64,
}

impl Carriers {
    pub const FIXTURE: Carriers =
        Carriers { order: CARRIER, delivery: DC, exit: EC, pair_delivery: pair::PDC, pair_exit: pair::PEC, taker: CARRIER };

    /// Every order-side carrier at `c`, the taker's at the fixture value. A pair entry's `exitCarrier` funds its exit's own
    /// deliveries (`IfdPairState::exit_carrier_needed`): three carriers, as the fixture's 2 / 6 KAS.
    pub const fn order_side(c: u64) -> Carriers {
        Carriers { order: c, delivery: c as i64, exit: c as i64, pair_delivery: c as i64, pair_exit: 3 * c as i64, taker: CARRIER }
    }
}

thread_local! {
    static CARRIERS: std::cell::Cell<Carriers> = const { std::cell::Cell::new(Carriers::FIXTURE) };
}

/// Runs `f` with the scenarios built at `c` (restored afterwards, also on a panic).
pub fn with_carriers<R>(c: Carriers, f: impl FnOnce() -> R) -> R {
    struct Restore(Carriers);
    impl Drop for Restore {
        fn drop(&mut self) {
            CARRIERS.with(|x| x.set(self.0));
        }
    }
    let _restore = Restore(CARRIERS.with(|x| x.replace(c)));
    f()
}

pub fn carriers() -> Carriers {
    CARRIERS.with(|x| x.get())
}
pub fn carrier() -> u64 {
    carriers().order
}
pub fn dc() -> i64 {
    carriers().delivery
}
pub fn ec() -> i64 {
    carriers().exit
}
pub fn taker_carrier() -> u64 {
    carriers().taker
}
/// Base units per whole token of the fixture tokens (the price denominator): prices and tips are per 1000 base units.
pub const SCALE: i64 = 1_000;
/// One whole fixture token in base units: the fixtures count amounts in whole tokens (`n * WHOLE`).
pub const WHOLE: i64 = SCALE;
/// Priority tip, sompi per whole token.
pub const TIP: i64 = 100_000;
pub const EXPIRY: i64 = 400_000_000;
pub const NO_EXPIRY: i64 = 499_999_999_999;
pub const NOW: u64 = 1_000_000;
pub const TOKEN_COV: [u8; 32] = [0x70; 32];
/// Second token of the two-token routes.
pub const TOKEN_B: [u8; 32] = [0x71; 32];
pub const EXT: [u8; 32] = [0xee; 32];

pub const P250: i64 = 250_000_000;
pub const P255: i64 = 255_000_000;
pub const P260: i64 = 260_000_000;
pub const P245: i64 = 245_000_000;

pub fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}
pub fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}
pub const MAKER_A: u8 = 1;
pub const MAKER_B: u8 = 2;
pub const MAKER_C: u8 = 3;
pub const TAKER: u8 = 4;
pub const MATCHER: u8 = 5;
pub const KEEPER: u8 = 6;
pub const FOUNDER: u8 = 7;
pub const MERCHANT: u8 = 8;

/// Every fixture key: x-only pubkey -> secret key.
pub fn keys() -> BTreeMap<[u8; 32], [u8; 32]> {
    (1..=40u8).map(|n| (pk(n), sk(n))).collect()
}

pub fn utxo(tag: u8, amount: u64, daa: u64, cov: Option<[u8; 32]>) -> Utxo {
    Utxo { transaction_id: [tag; 32], index: tag as u32, amount, block_daa_score: daa, covenant_id: cov }
}
pub fn key_utxo(tag: u8, key: u8, amount: u64) -> KeyUtxo {
    KeyUtxo { utxo: utxo(tag, amount, 500, None), pubkey: pk(key) }
}
pub fn cov(b: u8) -> [u8; 32] {
    [b; 32]
}

/// The extension commitment of the fixture token: KRON tokens have none.
pub fn ext_for(tpl: TemplateId) -> [u8; 32] {
    if tpl.family() == Family::Kron {
        [0; 32]
    } else {
        EXT
    }
}

pub fn tok_fields(tpl: TemplateId) -> ([u8; 32], i64, i64) {
    let t = token_template(tpl);
    (t.hash, t.prefix.len() as i64, t.suffix.len() as i64)
}
/// Default refund tip of a program.
/// (A program without a committed entry yet - the first regeneration of `data/keeper_tips.json` - gets a
/// placeholder: the tip value does not change the measured fee.)
pub fn rtip(tpl: TemplateId) -> i64 {
    tips(tpl).map(|t| t.refund_tip as i64).unwrap_or(10_000_000)
}
/// Default keeper tip of a program.
pub fn ktip(tpl: TemplateId) -> i64 {
    tips(tpl).map(|t| t.keeper_tip as i64).unwrap_or(5_000_000)
}
/// Default refund tip of a pair order on the program pair (a placeholder before the pair table has the pair).
pub fn prtip(a: TemplateId, b: TemplateId) -> i64 {
    kob_protocol::defaults::pair_tips(a, b).map(|t| t.refund_tip as i64).unwrap_or(10_000_000)
}
/// Default keeper tip of a pair order on the program pair.
pub fn pktip(a: TemplateId, b: TemplateId) -> i64 {
    kob_protocol::defaults::pair_tips(a, b).map(|t| t.keeper_tip as i64).unwrap_or(5_000_000)
}

/// Limit ask of 10 whole tokens (minimum fill one whole token).
pub fn ask(maker: u8, price: i64, tpl: TemplateId) -> AskState {
    let (h, p, s) = tok_fields(tpl);
    AskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
        amount_left: 10 * WHOLE,
        extension_commitment: ext_for(tpl),
    }
}
/// A limit ask of `n` whole tokens.
pub fn ask_n(maker: u8, price: i64, tpl: TemplateId, n: i64) -> AskState {
    AskState { amount_left: n * WHOLE, ..ask(maker, price, tpl) }
}
pub fn bid(maker: u8, price: i64, tpl: TemplateId) -> BidState {
    let (h, p, s) = tok_fields(tpl);
    BidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: ext_for(tpl),
        scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        reserve: 0,
        delivery_carrier: dc(),
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}
/// Market sell: an IOC auction from the touch 2.60 down to the 3% bound over 200 DAA,
/// activated 100 DAA before NOW (so it is halfway through its auction at NOW).
pub fn market_ask(maker: u8, tpl: TemplateId) -> AskState {
    AskState {
        tif: TIF_IOC,
        price: P260,
        price_end: P260 - P260 / 10_000 * 300,
        slope: (P260 / 10_000 * 300 + 199) / 200,
        decay_step: 1,
        active_from: NOW as i64 - 100,
        expiry_daa: NOW as i64 + 200,
        ..ask(maker, P260, tpl)
    }
}
/// Market buy: an IOC auction from the touch 2.40 up to the 3% bound over 200 DAA.
pub fn market_bid(maker: u8, tpl: TemplateId) -> BidState {
    let p = 240_000_000;
    BidState {
        tif: TIF_IOC,
        price: p,
        price_end: p + p / 10_000 * 300,
        slope: (p / 10_000 * 300 + 199) / 200,
        decay_step: 1,
        active_from: NOW as i64 - 100,
        expiry_daa: NOW as i64 + 200,
        ..bid(maker, p, tpl)
    }
}
/// OCO sell: take-profit 3.00, stop 2.00 with the default 3% band opening over 300 DAA, 10 whole tokens.
pub fn cond_ask(maker: u8, tpl: TemplateId) -> CondAskState {
    let (h, p, s) = tok_fields(tpl);
    CondAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        tp_price: 300_000_000,
        stop_price: 200_000_000,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 50,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(tpl),
        amount_left: 10 * WHOLE,
        parent: [0; 32],
        rpt_price: 0,
        rpt_until: 0,
        extension_commitment: ext_for(tpl),
    }
}
/// Buy OCO: limit leg 2.00, buy-stop 3.00 with the default 3% band over 300 DAA, 10 whole tokens.
pub fn cond_bid(maker: u8, tpl: TemplateId) -> CondBidState {
    let (h, p, s) = tok_fields(tpl);
    CondBidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: ext_for(tpl),
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        delivery_carrier: dc(),
        tp_price: 200_000_000,
        stop_price: 300_000_000,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 50,
        amount_left: 10 * WHOLE,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(tpl),
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    }
}
/// The exit every buy-first fixture commits to: OCO take-profit 3.00 / stop 2.20, GTC.
pub fn ifd_exit(maker: u8, tpl: TemplateId) -> CondAskState {
    CondAskState { stop_price: 220_000_000, expiry_daa: NO_EXPIRY, ..cond_ask(maker, tpl) }
}
/// Buy-first IFO entry at 2.60 for `n` whole tokens, minimum fill one whole token.
pub fn ifd_bid(maker: u8, n: i64, tpl: TemplateId) -> IfdBidState {
    let (h, p, s) = tok_fields(tpl);
    IfdBidState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: ext_for(tpl),
        scale: SCALE,
        amount_left: n * WHOLE,
        price: P260,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        delivery_carrier: dc(),
        exit_carrier: ec(),
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 50,
        keeper_tip: ktip(tpl),
        armed: 0,
        rpt_amount: 0,
        exit_state: IfdBidState::commit_exit(&ifd_exit(maker, tpl)),
    }
}
/// The exit every sell-first fixture commits to: buy-back limit 2.40 / buy-stop 2.80, GTC.
pub fn ifda_exit(maker: u8, tpl: TemplateId) -> CondBidState {
    CondBidState { tp_price: 240_000_000, stop_price: 280_000_000, expiry_daa: NO_EXPIRY, ..cond_bid(maker, tpl) }
}
/// Sell-first IFO entry at 2.50, 10 whole tokens, prefund 0.5 KAS per whole token, minimum fill one whole token.
pub fn ifd_ask(maker: u8, tpl: TemplateId) -> IfdAskState {
    let (h, p, s) = tok_fields(tpl);
    IfdAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        price: P250,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        prefund: KAS as i64 / 2,
        exit_carrier: ec(),
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 50,
        keeper_tip: ktip(tpl),
        armed: 0,
        amount_left: 10 * WHOLE,
        rpt_amount: 0,
        exit_state: IfdAskState::commit_exit_for(tpl.family(), &ifda_exit(maker, tpl)),
    }
}

pub fn tok_of(tag: u8, amount: i64, owner: [u8; 32], scheme: u8, daa: u64, token: [u8; 32]) -> TokenUtxo {
    let state = Kcc20State { amount, owner, owner_scheme: scheme, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment: EXT };
    TokenUtxo { utxo: utxo(tag, carrier(), daa, Some(token)), state: state.into() }
}
/// A key-owned token UTXO of the family of `prog` (KCC-20 P2PK / KRON address presence).
pub fn tok_on(prog: TemplateId, tag: u8, amount: i64, owner: [u8; 32], daa: u64) -> TokenUtxo {
    TokenUtxo { utxo: utxo(tag, carrier(), daa, Some(TOKEN_COV)), state: TokenState::user(prog.family(), amount, owner, EXT) }
}
/// Changes the owner of a token state (keeping its owner type).
pub fn set_owner(s: &mut TokenState, owner: [u8; 32]) {
    match s {
        TokenState::Kcc20(k) => k.owner = owner,
        TokenState::Kron(k) => k.owner = owner,
    }
}
/// Makes a token state covenant-owned (a stray of some order when the owner is an order id).
pub fn make_covenant_owned(s: &mut TokenState) {
    match s {
        TokenState::Kcc20(k) => k.owner_scheme = SCHEME_COVID,
        TokenState::Kron(k) => k.id_type = kob_protocol::family::KRON_TYPE_COVID,
    }
}
pub fn tok(tag: u8, amount: i64, owner: [u8; 32], scheme: u8, daa: u64) -> TokenUtxo {
    tok_of(tag, amount, owner, scheme, daa, TOKEN_COV)
}
/// A custody of `n` whole tokens owned by the order `order`.
pub fn custody(tag: u8, n: i64, order: [u8; 32], daa: u64) -> TokenUtxo {
    tok(tag, n * WHOLE, order, SCHEME_COVID, daa)
}
pub fn order<S>(tag: u8, amount: u64, c: [u8; 32], daa: u64, state: S) -> OrderUtxo<S> {
    OrderUtxo { utxo: utxo(tag, amount, daa, Some(c)), state }
}
pub fn fee() -> FeeOptions {
    FeeOptions::default()
}
/// Trigger evidence: a plain resting ask (maker C, cov 0x9a, 5 whole tokens, UTXO DAA 1000: exposed long before NOW)
/// quoting `price`, of which the batch buys `n` whole tokens.
pub fn ev_ask(price: i64, n: i64, tpl: TemplateId) -> Leg {
    let c = cov(0x9a);
    Leg::Ask {
        order: order(90, carrier(), c, 1_000, ask_n(MAKER_C, price, tpl, 5)),
        custody: custody(91, 5, c, 1_000),
        amount: n * WHOLE,
        t: None,
    }
}
/// Trigger evidence: a plain resting bid (maker C, cov 0x9b, UTXO DAA 1000) quoting `price`, into which the
/// batch sells `n` whole tokens.
pub fn ev_bid(price: i64, n: i64, tpl: TemplateId) -> Leg {
    let b = bid(MAKER_C, price, tpl);
    Leg::Bid { order: order(92, b.escrow(5 * WHOLE, 5).unwrap() as u64, cov(0x9b), 1_000, b), amount: n * WHOLE, t: None }
}
/// Amount and side of an evidence leg.
fn ev_amount(e: &Leg) -> (i64, bool) {
    match e {
        Leg::Ask { amount, .. } => (*amount, true),
        Leg::Bid { amount, .. } => (*amount, false),
        _ => panic!("evidence is a plain leg"),
    }
}
/// Adds an evidence leg at the end of a batch (its taker side: buys from an ask with the batch's funding,
/// sells taker tokens into a bid) and returns its leg index.
fn add_evidence(b: &mut Batch, e: Option<Leg>) -> Option<usize> {
    let e = e?;
    let (amount, ask) = ev_amount(&e);
    if !ask {
        let extra = tok(93, amount, pk(TAKER), SCHEME_P2PK, 1_000);
        if let Some(t) = b.taker_tokens.first_mut() {
            t.state = t.state.with_amount(t.state.amount() + amount);
        } else {
            b.taker_tokens.push(extra);
            b.change = b.change.or(Some(pk(TAKER)));
        }
    } else if b.funding.is_empty() {
        b.funding = vec![key_utxo(94, TAKER, 1_000 * KAS)];
    }
    b.legs.push(e);
    Some(b.legs.len() - 1)
}

fn batch(legs: Vec<Leg>) -> Batch {
    Batch {
        lock_time: NOW,
        legs,
        updates: vec![],
        taker_tokens: vec![],
        taker: None,
        taker_token_carrier: taker_carrier(),
        keep_surplus: vec![],
        keep_carrier: None,
        receivers: vec![],
        payments: vec![],
        funding: vec![],
        change: None,
        records: vec![],
        fee: fee(),
    }
}

const T8: TemplateId = TemplateId::Kcc20Ref8x8;

/// (price, whole tokens) of a bid / (price, whole tokens held, whole tokens sold) of an ask of the multi-order batch fixture.
type BidPlan = (i64, i64);
type AskPlan = (i64, i64, i64);

/// Taker buys `n` whole tokens from one ask (custody = its amountLeft).
fn take_ask_at(a: AskState, n: i64, daa: u64) -> Batch {
    let c = cov(0xa1);
    let held = a.amount_left;
    let mut b = batch(vec![Leg::Ask {
        order: order(10, carrier(), c, daa, a),
        custody: tok(11, held, c, SCHEME_COVID, daa),
        amount: n * WHOLE,
        t: None,
    }]);
    b.funding = vec![key_utxo(12, TAKER, 1_000 * KAS)];
    b
}
fn take_ask(a: AskState, n: i64) -> Batch {
    take_ask_at(a, n, 1_000)
}

/// Taker sells `n` whole tokens into one bid funded for `funded` whole tokens (one delivery).
fn take_bid(bs: BidState, n: i64, funded: i64) -> Batch {
    let v = bs.escrow(funded * WHOLE, 1).unwrap() as u64;
    let mut b = batch(vec![Leg::Bid { order: order(20, v, cov(0xb1), 1_000, bs), amount: n * WHOLE, t: None }]);
    b.taker_tokens = vec![tok(21, n * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    b
}

/// A taker buys `n` whole tokens of a conditional ask (cov 0xc1, UTXO DAA `daa`) on `leg`; `ev`: an evidence leg that
/// arms an unarmed stop in the same transaction (touch).
fn cond_fill_at(c: CondAskState, n: i64, leg: u8, ev: Option<Leg>, daa: u64) -> Batch {
    let id = cov(0xc1);
    let held = c.amount_left;
    let mut b = batch(vec![Leg::CondAsk {
        order: order(30, carrier(), id, daa, c),
        custody: tok(31, held, id, SCHEME_COVID, daa),
        amount: n * WHOLE,
        leg,
        evidence: None,
        t: None,
        merge: None,
    }]);
    b.funding = vec![key_utxo(33, TAKER, 1_000 * KAS)];
    let k = add_evidence(&mut b, ev);
    if let Leg::CondAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = k;
    }
    b
}
fn cond_fill(c: CondAskState, n: i64, leg: u8, ev: Option<Leg>) -> Batch {
    cond_fill_at(c, n, leg, ev, 2_000)
}

fn condb_fill_at(c: CondBidState, n: i64, leg: u8, ev: Option<Leg>, daa: u64) -> Batch {
    let v = c.escrow(2).unwrap() as u64;
    let mut b = batch(vec![Leg::CondBid {
        order: order(40, v, cov(0xe1), daa, c),
        amount: n * WHOLE,
        leg,
        evidence: None,
        t: None,
        merge: None,
    }]);
    b.taker_tokens = vec![tok(41, n * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    let k = add_evidence(&mut b, ev);
    if let Leg::CondBid { evidence, .. } = &mut b.legs[0] {
        *evidence = k;
    }
    b
}
fn condb_fill(c: CondBidState, n: i64, leg: u8, ev: Option<Leg>) -> Batch {
    condb_fill_at(c, n, leg, ev, 2_000)
}

/// A taker sells `n` whole tokens into a buy-first entry (cov 0xd1, UTXO DAA `daa`) funded with its escrow.
fn ifd_fill(s: IfdBidState, n: i64, ev: Option<Leg>, daa: u64) -> Batch {
    let v = s.escrow().unwrap() as u64;
    let mut b = batch(vec![Leg::IfdBid { order: order(50, v, cov(0xd1), daa, s), amount: n * WHOLE, evidence: None, t: None }]);
    b.taker_tokens = vec![tok(51, n * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
    b.change = Some(pk(TAKER));
    let k = add_evidence(&mut b, ev);
    if let Leg::IfdBid { evidence, .. } = &mut b.legs[0] {
        *evidence = k;
    }
    b
}

/// A taker buys `n` whole tokens from a sell-first entry (cov 0xf1) holding `s.amount_left`.
fn ifda_fill(s: IfdAskState, n: i64, value: u64, ev: Option<Leg>, daa: u64) -> Batch {
    let held = s.amount_left;
    let mut b = batch(vec![Leg::IfdAsk {
        order: order(60, value, cov(0xf1), daa, s),
        custody: tok(61, held, cov(0xf1), SCHEME_COVID, daa),
        amount: n * WHOLE,
        evidence: None,
        t: None,
    }]);
    b.funding = vec![key_utxo(62, TAKER, 1_000 * KAS)];
    let k = add_evidence(&mut b, ev);
    if let Leg::IfdAsk { evidence, .. } = &mut b.legs[0] {
        *evidence = k;
    }
    b
}

/// A matcher's batch that only fills the evidence leg and updates (arms / trails) `o` next to it (the
/// matcher takes the keeper tip).
fn update_batch(o: OrderUtxo<AnyState>, ev: Leg) -> Batch {
    let mut b = batch(vec![]);
    let k = add_evidence(&mut b, Some(ev)).expect("evidence");
    b.updates = vec![BatchUpdate { evidence_b: None, order: o, evidence: k, take: None }];
    b.change = Some(pk(MATCHER));
    b
}

/// rptUntil of an exit booked at NOW by an entry of UTXO DAA 1000.
pub fn booked_until() -> i64 {
    rpt_until(EXPIRY, 1_000).unwrap()
}

/// A booked buy-first exit (cov 0xc1) of the repeating entry 0xd1 with `n` whole tokens.
pub fn booked_ask_exit(tpl: TemplateId, n: i64) -> CondAskState {
    let e = IfdBidState { rpt_amount: 1 + 20 * WHOLE, ..ifd_bid(MAKER_A, 10, tpl) };
    e.exit_for(n * WHOLE, Some(Booking { parent: cov(0xd1), until: booked_until() })).unwrap()
}

/// A booked sell-first exit (cov 0xe1) of the repeating entry 0xf1 with `n` whole tokens.
pub fn booked_bid_exit(tpl: TemplateId, n: i64) -> CondBidState {
    let e = IfdAskState { rpt_amount: 1 + 20 * WHOLE, ..ifd_ask(MAKER_A, tpl) };
    e.exit_for(n * WHOLE, Some(Booking { parent: cov(0xf1), until: booked_until() })).unwrap()
}

/// Buy-first repeat take-profit: a taker buys `n` whole tokens of the booked exit at its TP
/// and the entry (`entry_n` whole tokens left) re-arms them (merge), unless `merge` is false.
fn rpt_bid_tp(tpl: TemplateId, exit: CondAskState, n: i64, entry_n: i64, merge: bool) -> Batch {
    let held = exit.amount_left;
    let e = IfdBidState { rpt_amount: 1 + 16 * WHOLE, amount_left: entry_n * WHOLE, ..ifd_bid(MAKER_A, 10, tpl) };
    let ev = (e.merge_budget(entry_n * WHOLE).unwrap() + entry_n * (dc() + ec()) + 2 * ec()) as u64;
    let mut b = batch(vec![Leg::CondAsk {
        order: order(30, carrier(), cov(0xc1), 2_000, exit),
        custody: tok(31, held, cov(0xc1), SCHEME_COVID, 2_000),
        amount: n * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: merge.then(|| order(50, ev, cov(0xd1), 1_000, e)),
    }]);
    b.funding = vec![key_utxo(33, TAKER, 1_000 * KAS)];
    b
}

/// Sell-first repeat take-profit: the booked exit buys back `n` whole tokens of its `exit.amount_left` from a
/// seller at its limit and the entry (`entry_n` whole tokens left) takes them into its custody.
fn rpt_ask_tp(tpl: TemplateId, exit: CondBidState, n: i64, entry_n: i64, merge: bool) -> Batch {
    let e = IfdAskState { rpt_amount: 1 + 16 * WHOLE, amount_left: entry_n * WHOLE, ..ifd_ask(MAKER_A, tpl) };
    let held = exit.amount_left;
    let xv = (e.proceeds(held, e.price).unwrap() + e.prefund_of(held).unwrap() + e.exit_carrier) as u64;
    let ev = (carrier() as i64 + e.prefund_of(entry_n * WHOLE).unwrap() + entry_n * ec()) as u64;
    let custody_utxo = (entry_n > 0).then(|| custody(61, entry_n, cov(0xf1), 1_000));
    let mut b = batch(vec![Leg::CondBid {
        order: order(40, xv, cov(0xe1), 2_000, exit),
        amount: n * WHOLE,
        leg: 0,
        evidence: None,
        t: None,
        merge: merge.then(|| SellFirstEntry { entry: order(60, ev, cov(0xf1), 1_000, e), custody: custody_utxo }),
    }]);
    b.taker_tokens = vec![tok(41, n * WHOLE, pk(TAKER), SCHEME_P2PK, 1_500)];
    b.change = Some(pk(TAKER));
    b
}

/// The golden-vector set: one request per builder shape (reference 3/3 token; the batch and one
/// creation use the 8/8 program KOB issues).
pub fn scenarios() -> Vec<(String, Action)> {
    scenarios_on(TemplateId::Kcc20Ref)
}

/// Every token program KOB builds for.
pub const PROGRAMS: [TemplateId; 8] = [
    TemplateId::Kcc20Ref,
    TemplateId::Kcc20Ref4x5,
    TemplateId::Kcc20Ref8x8,
    TemplateId::Kcc20Ref16x16,
    TemplateId::Kcc20P2,
    TemplateId::Kcc20KaspaCom025,
    TemplateId::KronToken2433,
    TemplateId::KronToken2732,
];

/// The KRON golden-vector set: every builder shape on the common KRON program.
pub fn scenarios_kron() -> Vec<(String, Action)> {
    scenarios_on(TemplateId::KronToken2433).into_iter().map(|(n, a)| (format!("kron.{n}"), a)).collect()
}

/// The scenarios with the given token program in place of the reference one.
pub fn scenarios_on(t3: TemplateId) -> Vec<(String, Action)> {
    let kron = t3.family() == Family::Kron;
    // The program of the creation / batch fixtures that exercise a large token program (KCC-20: 8/8).
    let t8 = if kron { t3 } else { T8 };
    let mut v: Vec<(String, Action)> = vec![];
    let mut add = |name: &str, a: Action| v.push((name.to_string(), a));

    // ------------------------------------------------ order creation
    let create = |order: AnyState, value: u64, tokens: Vec<TokenUtxo>| CreateOrder {
        order,
        value,
        tokens,
        token_carrier: carrier(),
        funding: vec![key_utxo(2, MAKER_A, 1_000 * KAS)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: fee(),
    };
    let maker_tokens = || vec![tok(1, 12 * WHOLE, pk(MAKER_A), SCHEME_P2PK, 500)];
    add("create.ask", Action::CreateOrder(create(AnyState::KobAsk(ask(MAKER_A, P250, t3)), carrier(), maker_tokens())));
    let mut tw = ask(MAKER_A, P250, t3);
    tw.interval = 600;
    tw.max_fill = 2 * WHOLE;
    add("create.ask.twap", Action::CreateOrder(create(AnyState::KobAsk(tw), carrier(), maker_tokens())));
    let mut du = ask(MAKER_A, 300_000_000, t3);
    du.slope = 1_000_000;
    du.price_end = 200_000_000;
    du.active_from = 1_000;
    add("create.ask.dutch", Action::CreateOrder(create(AnyState::KobAsk(du), carrier(), maker_tokens())));
    add("create.ask.market", Action::CreateOrder(create(AnyState::KobAsk(market_ask(MAKER_A, t3)), carrier(), maker_tokens())));
    let mut day = create(AnyState::KobAsk(ask(MAKER_A, P250, t3)), carrier(), maker_tokens());
    let d = kob_protocol::defaults::day_order(NOW, 1_790_694_000, Some(10_020));
    if let AnyState::KobAsk(a) = &mut day.order {
        a.expiry_daa = d.expiry_daa as i64;
    }
    day.deadline = Some(d.deadline);
    add("create.ask.day", Action::CreateOrder(day));
    let bs = bid(MAKER_A, P245, t3);
    let mut c = create(AnyState::KobBid(bs.clone()), bs.escrow(10 * WHOLE, 3).unwrap() as u64, vec![]);
    c.records = vec![Record::Note { text: "kob-web/0.1".into() }];
    add("create.bid", Action::CreateOrder(c));
    let mut dca = bid(MAKER_A, P245, t3);
    dca.interval = 600;
    dca.max_fill = WHOLE;
    add(
        "create.bid.dca",
        Action::CreateOrder(create(AnyState::KobBid(dca.clone()), dca.escrow(5 * WHOLE, 5).unwrap() as u64, vec![])),
    );
    let mb = market_bid(MAKER_A, t3);
    add(
        "create.bid.market",
        Action::CreateOrder(create(AnyState::KobBid(mb.clone()), mb.escrow(4 * WHOLE, 1).unwrap() as u64, vec![])),
    );
    add("create.condAsk", Action::CreateOrder(create(AnyState::KobCondAsk(cond_ask(MAKER_A, t3)), carrier(), maker_tokens())));
    let cb = cond_bid(MAKER_A, t3);
    add("create.condBid", Action::CreateOrder(create(AnyState::KobCondBid(cb.clone()), cb.escrow(2).unwrap() as u64, vec![])));
    let ib = IfdBidState { min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, t3) };
    add("create.ifdBid", Action::CreateOrder(create(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, vec![])));
    let ibs = IfdBidState { entry_stop: 255_000_000, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, t3) };
    add(
        "create.ifdBid.stopEntry",
        Action::CreateOrder(create(AnyState::KobIfdBid(ibs.clone()), ibs.escrow().unwrap() as u64, vec![])),
    );
    let ibr = IfdBidState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, t3) };
    add("create.ifdBid.repeat", Action::CreateOrder(create(AnyState::KobIfdBid(ibr.clone()), ibr.escrow().unwrap() as u64, vec![])));
    let ia = IfdAskState { min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    add(
        "create.ifdAsk",
        Action::CreateOrder(create(AnyState::KobIfdAsk(ia.clone()), ia.escrow(carrier() as i64).unwrap() as u64, maker_tokens())),
    );
    let iar = IfdAskState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    add(
        "create.ifdAsk.repeat",
        Action::CreateOrder(create(AnyState::KobIfdAsk(iar.clone()), iar.escrow(carrier() as i64).unwrap() as u64, maker_tokens())),
    );
    let mut c = create(AnyState::KobAsk(ask(MAKER_A, P250, t8)), carrier(), vec![]);
    c.tokens = vec![tok(1, 10 * WHOLE, pk(MAKER_A), SCHEME_P2PK, 500)];
    c.records = vec![Record::X402 { reference: vec![0x42; 32] }];
    add("create.ask.8x8.x402", Action::CreateOrder(c));

    // ------------------------------------------------ cancel, cancel-replace (incl. strays)
    let a_id = cov(0xa1);
    let cancel = |o: OrderUtxo<AnyState>, custody: Option<TokenUtxo>| CancelOrder {
        prefund: None,
        order: o,
        custody,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    };
    let ao = || order(10, carrier(), a_id, 1_000, AnyState::KobAsk(ask(MAKER_A, P250, t3)));
    add("cancel.ask", Action::CancelOrder(cancel(ao(), Some(custody(11, 10, a_id, 1_000)))));
    let mut cs = cancel(ao(), Some(custody(11, 10, a_id, 1_000)));
    cs.strays = vec![tok(13, 1, a_id, SCHEME_COVID, 1_200)];
    add("cancel.ask.sweepStray", Action::CancelOrder(cs));
    let bo = || order(20, 25 * KAS, cov(0xb1), 1_000, AnyState::KobBid(bid(MAKER_B, P245, t3)));
    add("cancel.bid", Action::CancelOrder(cancel(bo(), None)));
    let mut cs = cancel(bo(), None);
    cs.strays = vec![tok(22, 3 * WHOLE, cov(0xb1), SCHEME_COVID, 1_200)];
    add("cancel.bid.sweepStray", Action::CancelOrder(cs));
    add(
        "cancel.condAsk",
        Action::CancelOrder(cancel(
            order(30, carrier(), cov(0xc1), 2_000, AnyState::KobCondAsk(cond_ask(MAKER_A, t3))),
            Some(custody(31, 10, cov(0xc1), 2_000)),
        )),
    );
    let cbo = cond_bid(MAKER_B, t3);
    add(
        "cancel.condBid",
        Action::CancelOrder(cancel(order(40, cbo.escrow(2).unwrap() as u64, cov(0xe1), 2_000, AnyState::KobCondBid(cbo)), None)),
    );
    let ibo = ifd_bid(MAKER_A, 10, t3);
    add(
        "cancel.ifdBid",
        Action::CancelOrder(cancel(order(50, ibo.escrow().unwrap() as u64, cov(0xd1), 1_000, AnyState::KobIfdBid(ibo)), None)),
    );
    let iav = ifd_ask(MAKER_A, t3).escrow(carrier() as i64).unwrap() as u64;
    add(
        "cancel.ifdAsk",
        Action::CancelOrder(cancel(
            order(60, iav, cov(0xf1), 1_000, AnyState::KobIfdAsk(ifd_ask(MAKER_A, t3))),
            Some(custody(61, 10, cov(0xf1), 1_000)),
        )),
    );
    let empty = IfdAskState { amount_left: 0, rpt_amount: 1, ..ifd_ask(MAKER_A, t3) };
    add(
        "cancel.ifdAsk.emptyRepeat",
        Action::CancelOrder(cancel(order(60, carrier(), cov(0xf1), 1_000, AnyState::KobIfdAsk(empty.clone())), None)),
    );
    // "Cancel all" of a repeat position: the entry and its booked exits in one transaction.
    let entry_b = IfdBidState { rpt_amount: 1 + 16 * WHOLE, amount_left: 3 * WHOLE, ..ifd_bid(MAKER_A, 10, t3) };
    let exit_items = [(0xc1u8, 4i64, 30u8), (0xc2, 3, 34)].map(|(c, n, tag)| CancelItem {
        prefund: None,
        order: order(tag, carrier(), cov(c), 2_000, AnyState::KobCondAsk(booked_ask_exit(t3, n))),
        custody: Some(custody(tag + 1, n, cov(c), 2_000)),
        strays: vec![],
    });
    let mut orders = vec![CancelItem {
        prefund: None,
        order: order(50, entry_b.escrow().unwrap() as u64, cov(0xd1), 1_000, AnyState::KobIfdBid(entry_b)),
        custody: None,
        strays: vec![],
    }];
    orders.extend(exit_items);
    let position = |orders: Vec<CancelItem>| CancelPosition {
        orders,
        funding: vec![],
        change: None,
        lock_time: 0,
        token_carrier: None,
        records: vec![],
        fee: fee(),
    };
    add("cancel.position.repeatBuyFirst", Action::CancelPosition(position(orders)));
    let entry_a = IfdAskState { rpt_amount: 1 + 16 * WHOLE, amount_left: 6 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    let xb = booked_bid_exit(t3, 4);
    add(
        "cancel.position.repeatSellFirst",
        Action::CancelPosition(position(vec![
            CancelItem {
                prefund: None,
                order: order(60, entry_a.escrow(carrier() as i64).unwrap() as u64, cov(0xf1), 1_000, AnyState::KobIfdAsk(entry_a)),
                custody: Some(custody(61, 6, cov(0xf1), 1_000)),
                strays: vec![],
            },
            CancelItem {
                prefund: None,
                order: order(40, xb.escrow(1).unwrap() as u64, cov(0xe1), 2_000, AnyState::KobCondBid(xb)),
                custody: None,
                strays: vec![],
            },
        ])),
    );
    let mut cr = cancel(ao(), Some(custody(11, 10, a_id, 1_000)));
    cr.replace = Some(Replacement {
        order: AnyState::KobAsk(ask(MAKER_A, 240_000_000, t3)),
        value: carrier() - (carrier() / 2).min(20_000_000),
        token_carrier: None,
        deadline: None,
    });
    add("cancelReplace.ask", Action::CancelOrder(cr));
    // In-place amend: the maker's cancel continues the order's covenant id with the new terms; the custody (owned by
    // that id) stays where it is. Without funding the order's carrier pays the fee.
    let amend = |amended: AskState, funding: Vec<KeyUtxo>, deadline: Option<u64>| AmendOrder {
        order: ao(),
        amended: AnyState::KobAsk(amended),
        value: None,
        funding,
        change: None,
        lock_time: 0,
        deadline,
        records: vec![],
        fee: fee(),
    };
    add("amend.ask", Action::AmendOrder(amend(AskState { price: 240_000_000, ..ask(MAKER_A, P250, t3) }, vec![], None)));
    add(
        "amend.ask.funded",
        Action::AmendOrder(amend(
            AskState { price: 240_000_000, ..ask(MAKER_A, P250, t3) },
            vec![key_utxo(12, MAKER_A, 20 * KAS)],
            None,
        )),
    );
    // a funding UTXO that leaves a tiny change: it rides on the order instead (the change rule)
    add(
        "amend.ask.tinyChange",
        Action::AmendOrder(amend(
            AskState { price: 240_000_000, ..ask(MAKER_A, P250, t3) },
            vec![key_utxo(12, MAKER_A, KAS / 10)],
            None,
        )),
    );
    add(
        "amend.ask.day",
        Action::AmendOrder(amend(
            AskState { price: 255_000_000, tip: 2 * TIP, tif: TIF_IOC, ..ask(MAKER_A, P250, t3) },
            vec![],
            Some(1_790_726_400),
        )),
    );
    // In-place amend of a plain bid: its escrow pays the fee (no funding), or a funding UTXO tops the escrow up
    let amend_bid = |amended: BidState, value: Option<u64>, funding: Vec<KeyUtxo>| AmendOrder {
        order: bo(),
        amended: AnyState::KobBid(amended),
        value,
        funding,
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: fee(),
    };
    add("amend.bid", Action::AmendOrder(amend_bid(BidState { price: 240_000_000, ..bid(MAKER_B, P245, t3) }, None, vec![])));
    add(
        "amend.bid.funded",
        Action::AmendOrder(amend_bid(
            BidState { price: 250_000_000, tip: 2 * TIP, ..bid(MAKER_B, P245, t3) },
            Some(35 * KAS),
            vec![key_utxo(23, MAKER_B, 20 * KAS)],
        )),
    );
    let mut cr = cancel(ao(), Some(custody(11, 10, a_id, 1_000)));
    cr.replace = Some(Replacement {
        order: AnyState::KobCondAsk(CondAskState { amount_left: 6 * WHOLE, ..cond_ask(MAKER_A, t3) }),
        value: carrier(),
        token_carrier: Some(carrier()),
        deadline: None,
    });
    cr.funding = vec![key_utxo(12, MAKER_A, 20 * KAS)];
    add("cancelReplace.askToOco.partial", Action::CancelOrder(cr));
    let mut cr = cancel(ao(), Some(custody(11, 10, a_id, 1_000)));
    cr.tokens = vec![tok(14, 3 * WHOLE, pk(MAKER_A), SCHEME_P2PK, 900)];
    cr.funding = vec![key_utxo(12, MAKER_A, 20 * KAS)];
    cr.replace = Some(Replacement {
        order: AnyState::KobAsk(ask_n(MAKER_A, P250, t3, 12)),
        value: carrier(),
        token_carrier: None,
        deadline: None,
    });
    add("cancelReplace.ask.topUp", Action::CancelOrder(cr));
    let mut cr = cancel(bo(), None);
    cr.replace = Some(Replacement {
        order: AnyState::KobBid(bid(MAKER_B, 247_000_000, t3)),
        value: 24 * KAS,
        token_carrier: None,
        deadline: None,
    });
    add("cancelReplace.bid", Action::CancelOrder(cr));

    // ------------------------------------------------ refunds, kills, close (keepers paid by the tip)
    let refund = |o: OrderUtxo<AnyState>, custody: Option<TokenUtxo>, lock: u64| RefundOrder {
        prefund: None,
        order: o,
        foreign: vec![],
        custody,
        lock_time: lock,
        funding: vec![],
        change: None,
        fee: fee(),
    };
    let fresh = (EXPIRY - 1_000) as u64;
    add(
        "refund.ask.expiry",
        Action::RefundOrder(refund(
            order(10, carrier(), a_id, fresh, AnyState::KobAsk(ask(MAKER_A, P250, t3))),
            Some(custody(11, 10, a_id, fresh)),
            EXPIRY as u64,
        )),
    );
    let mut gtc = ask(MAKER_A, P250, t3);
    gtc.expiry_daa = NO_EXPIRY;
    let mut r = refund(
        order(10, carrier(), a_id, 1_000, AnyState::KobAsk(gtc)),
        Some(custody(11, 10, a_id, 1_000)),
        (1_000 + MAX_IDLE) as u64,
    );
    r.funding = vec![key_utxo(12, KEEPER, 5 * KAS)];
    r.change = Some(pk(KEEPER));
    add("refund.ask.idle90d.keeperChange", Action::RefundOrder(r));
    let ioc = AskState { tif: TIF_IOC, expiry_daa: NO_EXPIRY, ..ask(MAKER_A, P250, t3) };
    add(
        "refund.ask.iocKill",
        Action::RefundOrder(refund(
            order(10, carrier(), a_id, 1_000, AnyState::KobAsk(ioc)),
            Some(custody(11, 10, a_id, 1_000)),
            (1_000 + IOC_LIFE) as u64,
        )),
    );
    let fok = BidState { tif: TIF_FOK, active_from: 5_000, expiry_daa: NO_EXPIRY, ..bid(MAKER_B, P245, t3) };
    add(
        "refund.bid.fokKill",
        Action::RefundOrder(refund(order(20, 25 * KAS, cov(0xb1), 1_000, AnyState::KobBid(fok)), None, (5_000 + IOC_LIFE) as u64)),
    );
    add(
        "refund.bid",
        Action::RefundOrder(refund(
            order(20, 25 * KAS, cov(0xb1), fresh, AnyState::KobBid(bid(MAKER_B, P245, t3))),
            None,
            EXPIRY as u64,
        )),
    );
    add(
        "refund.condAsk",
        Action::RefundOrder(refund(
            order(30, carrier(), cov(0xc1), fresh, AnyState::KobCondAsk(cond_ask(MAKER_A, t3))),
            Some(custody(31, 10, cov(0xc1), fresh)),
            EXPIRY as u64,
        )),
    );
    let cbr = cond_bid(MAKER_B, t3);
    add(
        "refund.condBid",
        Action::RefundOrder(refund(
            order(40, cbr.escrow(2).unwrap() as u64, cov(0xe1), fresh, AnyState::KobCondBid(cbr)),
            None,
            EXPIRY as u64,
        )),
    );
    let ibr = ifd_bid(MAKER_A, 10, t3);
    add(
        "refund.ifdBid",
        Action::RefundOrder(refund(
            order(50, ibr.escrow().unwrap() as u64, cov(0xd1), fresh, AnyState::KobIfdBid(ibr)),
            None,
            EXPIRY as u64,
        )),
    );
    add(
        "refund.ifdAsk",
        Action::RefundOrder(refund(
            order(60, iav, cov(0xf1), fresh, AnyState::KobIfdAsk(ifd_ask(MAKER_A, t3))),
            Some(custody(61, 10, cov(0xf1), fresh)),
            EXPIRY as u64,
        )),
    );
    add(
        "close.ifdAsk.emptyRepeat",
        Action::RefundOrder(refund(order(60, carrier(), cov(0xf1), fresh, AnyState::KobIfdAsk(empty)), None, EXPIRY as u64)),
    );

    add(
        "send.tokens",
        Action::SendTokens(SendTokens {
            token: TokenRef { covenant_id: TOKEN_COV, program: t3 },
            tokens: vec![tok(1, 7 * WHOLE, pk(TAKER), SCHEME_P2PK, 500), tok(2, 5 * WHOLE, pk(TAKER), SCHEME_P2PK, 500)],
            recipients: vec![TokenRecipient { pubkey: pk(MAKER_C), amount: 9 * WHOLE, carrier: carrier() }],
            token_change: None,
            token_change_carrier: carrier(),
            funding: vec![key_utxo(3, TAKER, 10 * KAS)],
            change: None,
            records: vec![],
            fee: fee(),
        }),
    );

    // ------------------------------------------------ takers and matchers
    add("take.ask.partial", Action::Batch(take_ask(ask(MAKER_A, P250, t3), 4)));
    let mut z = ask(MAKER_A, P250, t3);
    z.tip = 0;
    add("take.ask.zeroTip", Action::Batch(take_ask(z, 4)));
    let mut ioc = ask(MAKER_A, P250, t3);
    ioc.tif = TIF_IOC;
    add("take.ask.ioc", Action::Batch(take_ask(ioc, 4)));
    let mut fok = ask(MAKER_A, P250, t3);
    fok.tif = TIF_FOK;
    add("take.ask.fok", Action::Batch(take_ask(fok, 10)));
    let mut act = ask(MAKER_A, P250, t3);
    act.active_from = NOW as i64;
    add("take.ask.timedActivation", Action::Batch(take_ask(act, 4)));
    let mut tw = ask(MAKER_A, P250, t3);
    tw.interval = 600;
    tw.max_fill = 2 * WHOLE;
    add("take.ask.twap", Action::Batch(take_ask(tw, 2)));
    let mut tws = ask(MAKER_A, 300_000_000, t3);
    tws.interval = 600;
    tws.max_fill = 2 * WHOLE;
    tws.slope = 1_000_000;
    tws.price_end = 200_000_000;
    tws.decay_step = 10;
    add("take.ask.twapSliceAuction", Action::Batch(take_ask_at(tws, 2, NOW - 1_000)));
    let mut du = ask(MAKER_A, 300_000_000, t3);
    du.slope = 1_000_000;
    du.price_end = 200_000_000;
    du.active_from = 1_000;
    let mut b = take_ask(du, 4);
    if let Leg::Ask { t, .. } = &mut b.legs[0] {
        *t = Some(51_000);
    }
    add("take.ask.dutch", Action::Batch(b));
    add("take.ask.market", Action::Batch(take_ask(market_ask(MAKER_A, t3), 4)));
    add("take.bid.partial", Action::Batch(take_bid(bid(MAKER_B, P245, t3), 4, 10)));
    let mut bi = bid(MAKER_B, P245, t3);
    bi.tif = TIF_IOC;
    add("take.bid.ioc", Action::Batch(take_bid(bi, 4, 10)));
    let mut bf = bid(MAKER_B, P245, t3);
    bf.tif = TIF_FOK;
    add("take.bid.fok", Action::Batch(take_bid(bf, 4, 4)));
    let mut dca = bid(MAKER_B, P245, t3);
    dca.interval = 600;
    dca.max_fill = WHOLE;
    add("take.bid.dca", Action::Batch(take_bid(dca, 1, 10)));
    let mut rise = bid(MAKER_B, 200_000_000, t3);
    rise.slope = 1_000_000;
    rise.price_end = P260;
    rise.active_from = 1_000;
    let mut b = take_bid(rise, 4, 10);
    if let Leg::Bid { t, .. } = &mut b.legs[0] {
        *t = Some(51_000);
    }
    add("take.bid.rising", Action::Batch(b));
    add("take.bid.market", Action::Batch(take_bid(market_bid(MAKER_B, t3), 4, 4)));

    // Taker sells 6 whole tokens into 2 bids (1 partial + 1 exhausted).
    let b1 = bid(MAKER_B, P245, t3);
    let b2 = bid(MAKER_C, P250, t3);
    let mut s3 = batch(vec![
        Leg::Bid { order: order(20, b1.escrow(10 * WHOLE, 3).unwrap() as u64, cov(0xb1), 1_000, b1), amount: 4 * WHOLE, t: None },
        Leg::Bid { order: order(21, b2.escrow(2 * WHOLE, 1).unwrap() as u64, cov(0xb2), 1_000, b2), amount: 2 * WHOLE, t: None },
    ]);
    s3.taker_tokens = vec![tok(22, 6 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)];
    s3.change = Some(pk(TAKER));
    add("take.sell.twoBids", Action::Batch(s3));

    // Matcher with no capital crosses 1 bid x 2 asks (spread + tips).
    let cross = |tip: i64| {
        let mut bp = bid(MAKER_B, P260, t3);
        bp.tip = tip;
        let mut ap = ask_n(MAKER_A, P250, t3, 5);
        ap.tip = tip;
        let mut cp = ask(MAKER_C, P255, t3);
        cp.tip = tip;
        let bv = bp.escrow(8 * WHOLE, 1).unwrap() as u64;
        let mut b = batch(vec![
            Leg::Bid { order: order(20, bv, cov(0xb1), 1_000, bp), amount: 8 * WHOLE, t: None },
            Leg::Ask {
                order: order(10, carrier(), cov(0xa1), 1_000, ap),
                custody: custody(11, 5, cov(0xa1), 1_000),
                amount: 5 * WHOLE,
                t: None,
            },
            Leg::Ask {
                order: order(13, carrier(), cov(0xa3), 1_000, cp),
                custody: custody(14, 10, cov(0xa3), 1_000),
                amount: 3 * WHOLE,
                t: None,
            },
        ]);
        b.change = Some(pk(MATCHER));
        b
    };
    add("match.cross.1x2", Action::Batch(cross(TIP)));
    add("match.cross.1x2.zeroTips", Action::Batch(cross(0)));

    // B1: 3 bids x 5 asks on an 8/8 token + matcher funding (KRON: 3 x 3, its 4 token inputs per
    // transaction bound), arming a listed stop sell with one of its asks as the evidence (update).
    let b1 = {
        let (bid_ps, ask_ps): (Vec<BidPlan>, Vec<AskPlan>) = if kron {
            (vec![(P260, 2), (258_000_000, 2), (256_000_000, 2)], vec![(P250, 2, 2), (252_000_000, 2, 2), (253_000_000, 2, 2)])
        } else {
            (
                vec![(P260, 4), (258_000_000, 3), (256_000_000, 5)],
                vec![(P250, 2, 2), (252_000_000, 2, 2), (253_000_000, 2, 2), (P255, 2, 2), (256_000_000, 10, 4)],
            )
        };
        let mut legs = vec![];
        for (i, (p, n)) in bid_ps.iter().enumerate() {
            let bp = bid([MAKER_A, MAKER_B, MAKER_C][i], *p, t8);
            let v = bp.escrow(n * WHOLE, 1).unwrap() as u64;
            legs.push(Leg::Bid { order: order(20 + i as u8, v, cov(0xb1 + i as u8), 1_000, bp), amount: n * WHOLE, t: None });
        }
        for (i, (p, held, sold)) in ask_ps.iter().enumerate() {
            let c = cov(0xa1 + i as u8);
            legs.push(Leg::Ask {
                order: order(30 + i as u8, carrier(), c, 1_000 + i as u64, ask_n(8 + i as u8, *p, t8, *held)),
                custody: custody(40 + i as u8, *held, c, 1_000 + i as u64),
                amount: sold * WHOLE,
                t: None,
            });
        }
        let mut b = batch(legs);
        // a stop sell at 2.52 (8/8 token) armed by the fill of the ask at 2.50 (leg 3)
        let stop = CondAskState { stop_price: 252_000_000, ..cond_ask(MAKER_A, t8) };
        b.updates = vec![BatchUpdate {
            evidence_b: None,
            order: order(70, carrier(), cov(0x70 + 0x10), 2_000, AnyState::KobCondAsk(stop)),
            evidence: 3,
            take: None,
        }];
        b.funding = vec![key_utxo(51, MATCHER, 10 * KAS)];
        b
    };
    add(if kron { "match.batch.3x3.arm" } else { "match.batch.3x5.8x8.arm" }, Action::Batch(b1));

    // Conditional sells: take-profit, stop armed in the fill (pays the stop), stop auction after
    // arming (armed = 1 and a carried origin), whole-band stop, stop-limit.
    let oco = cond_ask(MAKER_A, t3);
    // evidence: a resting ask at 1.98 (at or below the 2.00 stop) and a resting bid at 2.37 (trails a
    // 2.00 stop with step 0.05 and gap 0.10 by 5 steps), one whole token of each filled in the transaction
    let down = || ev_ask(198_000_000, 1, t3);
    let up = || ev_bid(237_000_000, 1, t3);
    add("cond.ask.takeProfit.partial", Action::Batch(cond_fill(oco.clone(), 4, 0, None)));
    add("cond.ask.stop.trigger", Action::Batch(cond_fill(oco.clone(), 4, 1, Some(down()))));
    add("cond.ask.stop.auction", Action::Batch(cond_fill_at(CondAskState { armed: 1, ..oco.clone() }, 4, 1, None, NOW - 100)));
    add("cond.ask.stop.auctionOrigin", Action::Batch(cond_fill(CondAskState { armed: NOW as i64 - 50, ..oco.clone() }, 4, 1, None)));
    add("cond.ask.stop.wholeBand", Action::Batch(cond_fill(CondAskState { armed: 1, band_daa: 0, ..oco.clone() }, 4, 1, None)));
    let mut sl = oco.clone();
    sl.tp_price = 0;
    add("cond.ask.stopLimit.trigger", Action::Batch(cond_fill(sl, 4, 1, Some(down()))));
    add("cond.ask.takeProfit.close", Action::Batch(cond_fill(oco.clone(), 10, 0, None)));
    // Updates inside a matcher's batch next to the evidence fill (paid by the order's keeperTip).
    let upd = |o: AnyState, value: u64, ev: Leg, tag: u8, seq_daa: u64| update_batch(order(tag, value, cov(tag), seq_daa, o), ev);
    add("cond.ask.update.arm", Action::Batch(upd(AnyState::KobCondAsk(oco.clone()), carrier(), down(), 70, 2_000)));
    let mut tr = oco.clone();
    tr.trail_step = 5_000_000;
    tr.trail_gap = 10_000_000;
    tr.trail_wait = 600;
    add("cond.ask.update.trail", Action::Batch(upd(AnyState::KobCondAsk(tr.clone()), carrier(), up(), 70, 2_000)));
    // a fill of the order's own take-profit leg is no evidence: the stop leg arms next to a plain fill only
    let mut both = upd(AnyState::KobCondAsk(oco.clone()), carrier(), down(), 70, 2_000);
    both.legs.push(ev_bid(237_000_000, 1, t3));
    both.updates.push(BatchUpdate {
        evidence_b: None,
        order: order(74, carrier(), cov(74), 2_000, AnyState::KobCondAsk(tr)),
        evidence: 1,
        take: None,
    });
    add("cond.ask.update.armAndTrail.oneBatch", Action::Batch(both));

    // Conditional buys.
    let boco = cond_bid(MAKER_B, t3);
    // evidence: a resting bid at 3.02 (at or above the 3.00 buy stop), a resting ask at 2.63 (trails it down)
    let arm = || ev_bid(302_000_000, 1, t3);
    let trail = || ev_ask(263_000_000, 1, t3);
    add("cond.bid.limit.partial", Action::Batch(condb_fill(boco.clone(), 4, 0, None)));
    add("cond.bid.stop.trigger", Action::Batch(condb_fill(boco.clone(), 4, 1, Some(arm()))));
    add(
        "cond.bid.stop.auction.close",
        Action::Batch(condb_fill_at(CondBidState { armed: 1, ..boco.clone() }, 10, 1, None, NOW - 150)),
    );
    add(
        "cond.bid.update.arm",
        Action::Batch(upd(AnyState::KobCondBid(boco.clone()), boco.escrow(2).unwrap() as u64, arm(), 80, 2_000)),
    );
    let mut trb = boco.clone();
    trb.trail_step = 5_000_000;
    trb.trail_gap = 10_000_000;
    trb.trail_wait = 600;
    add(
        "cond.bid.update.trail",
        Action::Batch(upd(AnyState::KobCondBid(trb.clone()), trb.escrow(2).unwrap() as u64, trail(), 80, 2_000)),
    );

    // If-done entries: partial and final fills, stop entries (armed in the fill, keeper arm,
    // auction after arming), repeating entries booking their exits.
    add("ifd.bid.partial", Action::Batch(ifd_fill(ifd_bid(MAKER_A, 10, t3), 4, None, 1_000)));
    add("ifd.bid.final", Action::Batch(ifd_fill(IfdBidState { min_fill: 4 * WHOLE, ..ifd_bid(MAKER_A, 3, t3) }, 3, None, 1_000)));
    let se = IfdBidState { entry_stop: 255_000_000, ..ifd_bid(MAKER_A, 10, t3) };
    let se_up = || ev_bid(256_000_000, 1, t3);
    add("ifd.bid.stopEntry.trigger", Action::Batch(ifd_fill(se.clone(), 4, Some(se_up()), 1_000)));
    add("ifd.bid.stopEntry.auction", Action::Batch(ifd_fill(IfdBidState { armed: 1, ..se.clone() }, 4, None, NOW - 150)));
    add("ifd.bid.update.arm", Action::Batch(upd(AnyState::KobIfdBid(se.clone()), se.escrow().unwrap() as u64, se_up(), 90, 1_000)));
    let rp = IfdBidState { rpt_amount: 1 + 20 * WHOLE, ..ifd_bid(MAKER_A, 10, t3) };
    add("ifd.bid.repeat.book", Action::Batch(ifd_fill(rp, 4, None, 1_000)));
    let rw = IfdBidState { rpt_amount: 1 + 4 * WHOLE, ..ifd_bid(MAKER_A, 4, t3) };
    add("ifd.bid.repeat.soldOutWaits", Action::Batch(ifd_fill(rw, 4, None, 1_000)));

    add("ifd.ask.partial", Action::Batch(ifda_fill(ifd_ask(MAKER_A, t3), 4, iav, None, 1_000)));
    let ia6 = IfdAskState { amount_left: 6 * WHOLE, min_fill: 7 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    add("ifd.ask.final", Action::Batch(ifda_fill(ia6.clone(), 6, ia6.escrow(carrier() as i64).unwrap() as u64, None, 1_000)));
    let sa = IfdAskState { entry_stop: P255, ..ifd_ask(MAKER_A, t3) };
    let sa_down = || ev_ask(254_000_000, 1, t3);
    let sav = sa.escrow(carrier() as i64).unwrap() as u64;
    add("ifd.ask.stopEntry.trigger", Action::Batch(ifda_fill(sa.clone(), 4, sav, Some(sa_down()), 1_000)));
    add("ifd.ask.stopEntry.auction", Action::Batch(ifda_fill(IfdAskState { armed: 1, ..sa.clone() }, 4, sav, None, NOW - 150)));
    add("ifd.ask.update.arm", Action::Batch(upd(AnyState::KobIfdAsk(sa), sav, sa_down(), 100, 1_000)));
    let rpa = IfdAskState { rpt_amount: 1 + 20 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    add("ifd.ask.repeat.book", Action::Batch(ifda_fill(rpa.clone(), 4, rpa.escrow(carrier() as i64).unwrap() as u64, None, 1_000)));
    let rwa = IfdAskState { amount_left: 4 * WHOLE, rpt_amount: 1 + 4 * WHOLE, ..ifd_ask(MAKER_A, t3) };
    add(
        "ifd.ask.repeat.soldOutWaits",
        Action::Batch(ifda_fill(rwa.clone(), 4, rwa.escrow(carrier() as i64).unwrap() as u64, None, 1_000)),
    );

    // Repeat IFD, buy-first: take-profit + merge (partial, sell-out, into an empty entry), the
    // plain take-profit after rptUntil, and a stop-loss that leaves the cycle.
    add("rpt.bid.merge.partial", Action::Batch(rpt_bid_tp(t3, booked_ask_exit(t3, 4), 3, 6, true)));
    add("rpt.bid.merge.sellOut", Action::Batch(rpt_bid_tp(t3, booked_ask_exit(t3, 4), 4, 6, true)));
    add("rpt.bid.merge.emptyEntry", Action::Batch(rpt_bid_tp(t3, booked_ask_exit(t3, 4), 4, 0, true)));
    let late = CondAskState { rpt_until: NOW as i64 - 1, ..booked_ask_exit(t3, 4) };
    add("rpt.bid.takeProfit.afterUntil", Action::Batch(rpt_bid_tp(t3, late, 4, 0, false)));
    add("rpt.bid.stopLoss", Action::Batch(cond_fill(booked_ask_exit(t3, 10), 4, 1, Some(down()))));
    // Repeat IFD, sell-first: buy-back + merge into the entry's custody, into a new custody.
    add("rpt.ask.merge.partial", Action::Batch(rpt_ask_tp(t3, booked_bid_exit(t3, 4), 3, 6, true)));
    add("rpt.ask.merge.newCustody", Action::Batch(rpt_ask_tp(t3, booked_bid_exit(t3, 4), 4, 0, true)));
    let late_b = CondBidState { rpt_until: NOW as i64 - 1, ..booked_bid_exit(t3, 4) };
    add("rpt.ask.takeProfit.afterUntil", Action::Batch(rpt_ask_tp(t3, late_b, 4, 0, false)));

    // Two-token routes: A -> KAS -> B in one transaction (swap, swap-and-pay with x402).
    let route = |pay: bool| {
        let bs = bid(MAKER_B, P245, t3);
        let bv = bs.escrow(5 * WHOLE, 1).unwrap() as u64;
        let a_b = AskState { token_cov_id: TOKEN_B, ..ask_n(MAKER_C, P250, t3, 5) };
        SwapRoute {
            lock_time: NOW,
            sell: vec![Leg::Bid { order: order(20, bv, cov(0xb1), 1_000, bs), amount: 3 * WHOLE, t: None }],
            buy: vec![Leg::Ask {
                order: order(10, carrier(), cov(0x5a), 1_000, a_b),
                custody: tok_of(11, 5 * WHOLE, cov(0x5a), SCHEME_COVID, 1_000, TOKEN_B),
                amount: 2 * WHOLE,
                t: None,
            }],
            tokens: vec![tok(21, 3 * WHOLE, pk(TAKER), SCHEME_P2PK, 1_000)],
            receiver: Some(pk(MERCHANT)),
            token_carrier: taker_carrier(),
            payments: if pay {
                vec![Payment { script_public_key: spk_to_string(&kob_protocol::script::p2pk_spk(&pk(MERCHANT))), amount: KAS }]
            } else {
                vec![]
            },
            funding: vec![key_utxo(12, TAKER, 20 * KAS)],
            change: Some(pk(TAKER)),
            records: if pay { vec![Record::X402 { reference: vec![0x99; 16] }] } else { vec![] },
            fee: fee(),
        }
    };
    add("route.swap", Action::SwapRoute(route(false)));
    add("route.swapAndPay.x402", Action::SwapRoute(route(true)));

    if kron {
        return v.into_iter().map(|(n, a)| (n, kron_action(a))).collect();
    }
    v
}

// ---------------------------------------------------------------- KRON conversion of the fixtures

/// Converts a KCC-20 fixture request into its KRON counterpart (see the module docs).
pub fn kron_action(a: Action) -> Action {
    let mut v = serde_json::to_value(&a).expect("action json");
    to_kron_json(&mut v);
    let mut out: Action = serde_json::from_value(v).unwrap_or_else(|e| panic!("KRON conversion of a fixture: {e}"));
    fund_presence(&mut out);
    out
}

fn to_kron_json(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            if let Some(scheme) = m.get("owner_scheme").and_then(|x| x.as_u64()) {
                // A KCC-20 token state -> the KRON layout: owner scheme 0 (P2PK) = address presence, 4 = covenant id.
                let id_type: u64 = match scheme {
                    0 => 3,
                    4 => 2,
                    other => panic!("fixture token owner scheme {other}"),
                };
                let (owner, amount) = (m["owner"].clone(), m["amount"].clone());
                m.clear();
                m.insert("owner".into(), owner);
                m.insert("id_type".into(), id_type.into());
                m.insert("amount".into(), amount);
                m.insert("is_minter".into(), 0.into());
                return;
            }
            if m.len() == 2 && m.contains_key("state") {
                if let Some(k) = m.get("kind").and_then(|k| k.as_str()) {
                    // (the pair kinds are one template for both families: the families of their tokens are state fields)
                    if k.starts_with("Kob") && !k.ends_with("Kron") && !k.contains("Pair") {
                        let kron = format!("{k}Kron");
                        m.insert("kind".into(), kron.into());
                    }
                }
            }
            for x in m.values_mut() {
                to_kron_json(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(to_kron_json),
        _ => {}
    }
}

/// KRON authorises an address-presence token input by a P2PK input of the owner: give every batch
/// whose taker tokens are held by a key without funding one funding input of that key.
fn fund_presence(a: &mut Action) {
    if let Action::Batch(b) = a {
        let owners: Vec<[u8; 32]> = b.taker_tokens.iter().map(|t| t.state.owner()).collect();
        for (k, o) in owners.into_iter().enumerate() {
            if !b.funding.iter().any(|f| f.pubkey == o) {
                b.funding.push(KeyUtxo { utxo: utxo(230 + k as u8, 2 * KAS, 500, None), pubkey: o });
            }
        }
    }
}

fn tw_ask(t3: TemplateId) -> AskState {
    let mut tw = ask(MAKER_A, P250, t3);
    tw.interval = 600;
    tw.max_fill = 2 * WHOLE;
    tw
}

/// Token slot grid for the budget table: for every token program and every (inputs, outputs)
/// shape, a covenant-owned sweep (asks sold out into bids + taker) and a P2PK transfer.
pub fn grid() -> Vec<(String, Action)> {
    let mut v = vec![];
    for tpl in PROGRAMS {
        let (mi, mo) = tpl.token_slots().unwrap();
        let start = v.len();
        for i in 1..=mi {
            for o in 1..=mo {
                // P2PK transfer: i inputs, o outputs.
                let tokens: Vec<TokenUtxo> = (0..i).map(|k| tok(100 + k as u8, 2 * WHOLE, pk(TAKER), SCHEME_P2PK, 500)).collect();
                let total = 2 * WHOLE * i as i64;
                let recipients: Vec<TokenRecipient> = (0..o)
                    .map(|k| TokenRecipient {
                        pubkey: pk(10 + k as u8),
                        amount: if k + 1 == o { total - (o as i64 - 1) } else { 1 },
                        carrier: carrier(),
                    })
                    .collect();
                v.push((
                    format!("grid.send.{}.i{i}.o{o}", tpl.name()),
                    Action::SendTokens(SendTokens {
                        token: TokenRef { covenant_id: TOKEN_COV, program: tpl },
                        tokens,
                        recipients,
                        token_change: None,
                        token_change_carrier: carrier(),
                        funding: vec![key_utxo(99, TAKER, 1_000 * KAS)],
                        change: None,
                        records: vec![],
                        fee: fee(),
                    }),
                ));
                // Covenant-owned sweep: i asks sold out (one whole token each, the last carrying the rest),
                // o - 1 bids of one whole token, and the taker output. Orders refuse more than 8 token inputs.
                if i > tpl.family().max_tok_in() {
                    continue;
                }
                let n_total = (i.max(o)) as i64;
                let mut legs = vec![];
                for k in 0..o - 1 {
                    let bp = bid(10 + k as u8, P260, tpl);
                    legs.push(Leg::Bid {
                        order: order(150 + k as u8, bp.escrow(WHOLE, 1).unwrap() as u64, cov(0x30 + k as u8), 1_000, bp),
                        amount: WHOLE,
                        t: None,
                    });
                }
                for k in 0..i {
                    let n = if k + 1 == i { n_total - (i as i64 - 1) } else { 1 };
                    let c = cov(0x60 + k as u8);
                    legs.push(Leg::Ask {
                        order: order(180 + k as u8, carrier(), c, 1_000, ask_n(30 + k as u8, P250, tpl, n)),
                        custody: custody(210 + k as u8, n, c, 1_000),
                        amount: n * WHOLE,
                        t: None,
                    });
                }
                let mut b = batch(legs);
                b.funding = vec![key_utxo(99, MATCHER, 1_000 * KAS)];
                v.push((format!("grid.sweep.{}.i{i}.o{o}", tpl.name()), Action::Batch(b)));
            }
        }
        if tpl.family() == Family::Kron {
            for (_, a) in &mut v[start..] {
                *a = kron_action(a.clone());
            }
        }
    }
    v
}

// ---------------------------------------------------------------- batch context padding (budget table)
//
// An order covenant's script-unit cost depends on the transaction around it, not only on its own leg:
// every other token input in the transaction (an ask's custody) adds about 120 units to each order
// input, every other bid a few. The budget table therefore measures every batch scenario also inside
// the largest batches the builders accept: padded with extra plain asks of one whole token up to the token slot
// limit, and with extra plain bids of one whole token.

fn pad_utxo(kind: u8, j: usize, amount: u64, cov: Option<[u8; 32]>) -> Utxo {
    let mut id = [0x5c; 32];
    id[0] = kind;
    id[1] = j as u8;
    Utxo { transaction_id: id, index: j as u32, amount, block_daa_score: 1_000, covenant_id: cov }
}

fn pad_cov(kind: u8, j: usize) -> [u8; 32] {
    let mut c = [0xc7; 32];
    c[0] = kind;
    c[1] = j as u8;
    c
}

/// The KCC-20 or KRON form of pad legs (built for KCC-20 states, converted like the fixtures).
fn in_family(t: TemplateId, pad: Batch) -> Batch {
    if t.family() != Family::Kron {
        return pad;
    }
    match kron_action(Action::Batch(pad)) {
        Action::Batch(b) => b,
        _ => unreachable!(),
    }
}

/// `a` with `asks` extra plain asks of one whole token (order input + custody token input each) and `bids` extra plain
/// bids of one whole token (the taker sells them from one extra P2PK token input), appended after the scenario's legs;
/// `None` for a request that is not a batch.
pub fn pad_batch(t: TemplateId, a: &Action, asks: usize, bids: usize) -> Option<Action> {
    pad_batch_shaped(t, a, asks, bids, WHOLE, P250)
}

/// [`pad_batch`] with pad legs of `amount` base units each at the ask price `price` (bids at `price + 0.10 KAS`): the
/// covenants' script-unit cost also depends on the magnitude of the amounts and prices around them (the split
/// multiplication of the quote rule), so the table is measured over both the one-token and a wide shape.
pub fn pad_batch_shaped(t: TemplateId, a: &Action, asks: usize, bids: usize, amount: i64, price: i64) -> Option<Action> {
    let Action::Batch(b0) = a else { return None };
    let mut legs = vec![];
    for j in 0..asks {
        let c = pad_cov(0xa7, j);
        let s = AskState { amount_left: amount, min_fill: amount, ..ask(30 + (j % 10) as u8, price, t) };
        let custody = TokenUtxo {
            utxo: pad_utxo(0xa8, j, carrier(), Some(TOKEN_COV)),
            state: TokenState::custody(Family::Kcc20, amount, c, EXT),
        };
        legs.push(Leg::Ask { order: OrderUtxo { utxo: pad_utxo(0xa7, j, carrier(), Some(c)), state: s }, custody, amount, t: None });
    }
    for j in 0..bids {
        let s = BidState { min_fill: amount, ..bid(20 + (j % 10) as u8, price + 10_000_000, t) };
        let value = s.escrow(amount, 1).expect("pad bid escrow") as u64;
        legs.push(Leg::Bid { order: OrderUtxo { utxo: pad_utxo(0xb7, j, value, Some(pad_cov(0xb7, j))), state: s }, amount, t: None });
    }
    let mut pad = batch(legs);
    if bids > 0 {
        pad.taker_tokens = vec![TokenUtxo {
            utxo: pad_utxo(0xb8, 0, carrier(), Some(TOKEN_COV)),
            state: TokenState::user(Family::Kcc20, bids as i64 * amount, pk(TAKER), EXT),
        }];
    }
    let pad = in_family(t, pad);
    let mut b = b0.clone();
    b.legs.extend(pad.legs);
    b.taker_tokens.extend(pad.taker_tokens);
    b.funding.push(KeyUtxo {
        utxo: pad_utxo(0xb9, 0, if amount == WHOLE && price == P250 { 1_000 * KAS } else { 2_000_000_000 * KAS }, None),
        pubkey: pk(TAKER),
    });
    if b.taker.is_none() {
        b.taker = Some(pk(TAKER));
    }
    if b.change.is_none() {
        b.change = Some(pk(TAKER));
    }
    Some(Action::Batch(b))
}

/// `a` with `asks` extra plain asks of `token` (program `t`) of one whole token each (an order input and a custody token
/// input each; the batch's taker receives what they sell), appended after the scenario's legs: the pad of the pair shapes,
/// whose covenants scan the inputs of both their tokens. `kind` keeps the pad covenant ids of two tokens apart.
pub fn pad_asks_of(t: TemplateId, token: [u8; 32], kind: u8, a: &Action, asks: usize) -> Option<Action> {
    let Action::Batch(b0) = a else { return None };
    let mut legs = vec![];
    for j in 0..asks {
        let c = pad_cov(kind, j);
        let s = AskState { token_cov_id: token, amount_left: WHOLE, min_fill: WHOLE, ..ask(30 + (j % 10) as u8, P250, t) };
        let custody = TokenUtxo {
            utxo: pad_utxo(kind + 1, j, carrier(), Some(token)),
            state: TokenState::custody(Family::Kcc20, WHOLE, c, EXT),
        };
        legs.push(Leg::Ask {
            order: OrderUtxo { utxo: pad_utxo(kind, j, carrier(), Some(c)), state: s },
            custody,
            amount: WHOLE,
            t: None,
        });
    }
    let pad = in_family(t, batch(legs));
    let mut b = b0.clone();
    b.legs.extend(pad.legs);
    b.funding.push(KeyUtxo { utxo: pad_utxo(kind + 2, 0, 1_000 * KAS, None), pubkey: pk(TAKER) });
    if b.taker.is_none() {
        b.taker = Some(pk(TAKER));
    }
    if b.change.is_none() {
        b.change = Some(pk(TAKER));
    }
    Some(Action::Batch(b))
}
