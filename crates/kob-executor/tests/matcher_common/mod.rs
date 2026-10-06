//! Book fixtures for the matcher and keeper tests: listed orders of every kind with exact custody,
//! operator funding, and a tick runner that validates every prepared transaction in the
//! rusty-kaspa v2.1.0 script engine.
#![allow(dead_code)]

pub mod pair;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};

use kob_executor::matcher::book::*;
use kob_executor::matcher::engine::{tick, EngineConfig, Prepared, TickInput, TickReport};
use kob_executor::matcher::family::{Families, Family};
use kob_executor::matcher::wallet::LocalKeys;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::defaults::tips;
use kob_protocol::state::*;
use kob_protocol::tx::*;

use kob_executor::testkit::{kron as kron_state, TOKEN_COV_KRON};

pub const KAS: u64 = 100_000_000;
pub const CARRIER: u64 = 10 * KAS;
pub const DC: i64 = 10 * KAS as i64;
pub const EC: i64 = 10 * KAS as i64;
/// Base units per whole fixture token (every fixture order's `scale`): prices and tips are per whole token.
pub const SCALE: i64 = 1_000;
/// One whole fixture token in base units: the fixtures count amounts in whole tokens (`n * WHOLE`).
pub const WHOLE: i64 = SCALE;
pub const TIP: i64 = 100_000;
pub const EXPIRY: i64 = 400_000_000;
pub const NO_EXPIRY: i64 = 499_999_999_999;
pub const NOW: u64 = 1_000_000;
pub const UTC: u64 = 1_790_694_000;
pub const TOKEN: [u8; 32] = [0x70; 32];
pub const EXT: [u8; 32] = [0xee; 32];
pub const T3: TemplateId = TemplateId::Kcc20Ref;
pub const T8: TemplateId = TemplateId::Kcc20Ref8x8;

pub const P250: i64 = 250_000_000;
pub const P255: i64 = 255_000_000;
pub const P260: i64 = 260_000_000;
pub const P245: i64 = 245_000_000;

pub const MATCHER: u8 = 5;
pub const KEEPER: u8 = 6;

pub fn sk(n: u8) -> [u8; 32] {
    [n; 32]
}
pub fn pk(n: u8) -> [u8; 32] {
    pubkey_of(&sk(n)).unwrap()
}

static NEXT: AtomicU32 = AtomicU32::new(1);

/// A fresh synthetic transaction id.
pub fn txid() -> [u8; 32] {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut t = [0u8; 32];
    t[..4].copy_from_slice(&n.to_le_bytes());
    t[31] = 0x5a;
    t
}

pub fn utxo(amount: u64, daa: u64, cov: Option<[u8; 32]>) -> Utxo {
    Utxo { transaction_id: txid(), index: 0, amount, block_daa_score: daa, covenant_id: cov }
}

/// A distinct covenant id per order.
pub fn cid(n: u32) -> [u8; 32] {
    let mut c = [0x33u8; 32];
    c[..4].copy_from_slice(&n.to_le_bytes());
    c
}

pub fn tok_fields(tpl: TemplateId) -> ([u8; 32], i64, i64) {
    kob_executor::testkit::tok_fields(tpl)
}
pub fn rtip(tpl: TemplateId) -> i64 {
    tips(tpl).unwrap().refund_tip as i64
}
pub fn ktip(tpl: TemplateId) -> i64 {
    tips(tpl).unwrap().keeper_tip as i64
}

/// A plain GTC ask of `amount` base units at `price` sompi per whole token (minimum fill one whole token).
pub fn ask(maker: u8, price: i64, amount: i64, tpl: TemplateId) -> AskState {
    let (h, p, s) = tok_fields(tpl);
    AskState {
        maker: pk(maker),
        token_cov_id: TOKEN,
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
        amount_left: amount,
    }
}

pub fn bid(maker: u8, price: i64, tpl: TemplateId) -> BidState {
    let (h, p, s) = tok_fields(tpl);
    BidState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: WHOLE,
        price,
        tip: TIP,
        tif: 0,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        reserve: 0,
        delivery_carrier: DC,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1_000,
    }
}

/// Market sell: IOC auction from `top` down 3% over 200 DAA from `active_from`.
pub fn market_ask(maker: u8, top: i64, amount: i64, active_from: i64, tpl: TemplateId) -> AskState {
    AskState {
        tif: TIF_IOC,
        price: top,
        price_end: top - top / 10_000 * 300,
        slope: (top / 10_000 * 300 + 199) / 200,
        decay_step: 1,
        active_from,
        expiry_daa: active_from + 300,
        ..ask(maker, top, amount, tpl)
    }
}

/// Market buy: IOC auction from `bottom` up 3% over 200 DAA.
pub fn market_bid(maker: u8, bottom: i64, active_from: i64, tpl: TemplateId) -> BidState {
    BidState {
        tif: TIF_IOC,
        price: bottom,
        price_end: bottom + bottom / 10_000 * 300,
        slope: (bottom / 10_000 * 300 + 199) / 200,
        decay_step: 1,
        active_from,
        expiry_daa: active_from + 300,
        ..bid(maker, bottom, tpl)
    }
}

pub fn cond_ask(maker: u8, tp: i64, stop: i64, amount: i64, tpl: TemplateId) -> CondAskState {
    let (h, p, s) = tok_fields(tpl);
    CondAskState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        tp_price: tp,
        stop_price: stop,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 600,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(tpl),
        amount_left: amount,
        parent: [0; 32],
        rpt_price: 0,
        rpt_until: 0,
    }
}

pub fn cond_bid(maker: u8, limit: i64, stop: i64, amount: i64, tpl: TemplateId) -> CondBidState {
    let (h, p, s) = tok_fields(tpl);
    CondBidState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        min_fill: WHOLE,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        delivery_carrier: DC,
        tp_price: limit,
        stop_price: stop,
        slip_bps: 300,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: 1,
        min_rest_daa: 600,
        amount_left: amount,
        armed: 0,
        band_daa: 300,
        keeper_tip: ktip(tpl),
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    }
}

/// Buy-first entry at `price` committing to exit TP 3.00 / stop 2.20 (GTC).
pub fn ifd_bid(maker: u8, price: i64, amount: i64, tpl: TemplateId) -> IfdBidState {
    let (h, p, s) = tok_fields(tpl);
    let exit =
        CondAskState { stop_price: 220_000_000, expiry_daa: NO_EXPIRY, ..cond_ask(maker, 300_000_000, 220_000_000, WHOLE, tpl) };
    IfdBidState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        extension_commitment: EXT,
        scale: SCALE,
        amount_left: amount,
        price,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        delivery_carrier: DC,
        exit_carrier: EC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 600,
        keeper_tip: ktip(tpl),
        armed: 0,
        rpt_amount: 0,
        exit_state: IfdBidState::commit_exit(&exit),
    }
}

/// Sell-first entry at `price` committing to exit buy-back 2.40 / buy-stop 2.80 (GTC).
pub fn ifd_ask(maker: u8, price: i64, amount: i64, tpl: TemplateId) -> IfdAskState {
    let (h, p, s) = tok_fields(tpl);
    let exit = CondBidState { expiry_daa: NO_EXPIRY, ..cond_bid(maker, 240_000_000, 280_000_000, WHOLE, tpl) };
    IfdAskState {
        maker: pk(maker),
        token_cov_id: TOKEN,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        price,
        tip: TIP,
        active_from: 0,
        expiry_daa: EXPIRY,
        refund_tip: rtip(tpl),
        prefund: KAS as i64 / 2,
        exit_carrier: EC,
        min_fill: WHOLE,
        entry_stop: 0,
        band_daa: 300,
        min_touch: 1,
        min_rest_daa: 600,
        keeper_tip: ktip(tpl),
        armed: 0,
        amount_left: amount,
        rpt_amount: 0,
        exit_state: IfdAskState::commit_exit(&exit),
    }
}

pub fn custody(amount: i64, owner: [u8; 32], daa: u64) -> TokenUtxo {
    let state = Kcc20State::custody(amount, owner, EXT).into();
    TokenUtxo { utxo: utxo(CARRIER, daa, Some(TOKEN)), state }
}

pub fn p2pk_tokens(amount: i64, owner: [u8; 32], daa: u64) -> TokenUtxo {
    TokenUtxo { utxo: utxo(CARRIER, daa, Some(TOKEN)), state: Kcc20State::p2pk(amount, owner, EXT).into() }
}

/// A listed order with the given UTXO value and DAA; ask-side kinds get an exact custody.
pub fn listed(id: [u8; 32], state: AnyState, value: u64, daa: u64) -> ListedOrder {
    let custody = state.custody_amount().filter(|a| *a > 0).map(|a| custody(a, id, daa));
    ListedOrder {
        family: Family::Kcc20,
        order: OrderUtxo { utxo: utxo(value, daa, Some(id)), state },
        custody,
        custody_b: None,
        deadline: None,
        seen_daa: daa,
        foreign: vec![],
        strays: vec![],
    }
}

pub fn l_ask(id: u32, s: AskState) -> ListedOrder {
    listed(cid(id), AnyState::KobAsk(s), CARRIER, 1_000)
}
/// A bid funded for `amount` base units: its buying power is exactly `amount` (the consumed budget
/// `ceil(amount·(pMax + tip)/scale)`, one delivery carrier and the reserve).
pub fn l_bid(id: u32, s: BidState, amount: i64) -> ListedOrder {
    let v = (s.used(amount).expect("budget") + s.delivery_carrier + s.reserve) as u64;
    listed(cid(id), AnyState::KobBid(s), v, 1_000)
}
pub fn l_cond_ask(id: u32, s: CondAskState, daa: u64) -> ListedOrder {
    listed(cid(id), AnyState::KobCondAsk(s), CARRIER, daa)
}
pub fn l_cond_bid(id: u32, s: CondBidState, daa: u64) -> ListedOrder {
    let v = s.escrow(s.max_fills()).expect("escrow") as u64;
    listed(cid(id), AnyState::KobCondBid(s), v, daa)
}
pub fn l_ifd_bid(id: u32, s: IfdBidState, daa: u64) -> ListedOrder {
    let v = s.escrow().expect("escrow") as u64;
    listed(cid(id), AnyState::KobIfdBid(s), v, daa)
}
pub fn l_ifd_ask(id: u32, s: IfdAskState, daa: u64) -> ListedOrder {
    let v = s.escrow(CARRIER as i64).expect("escrow") as u64;
    listed(cid(id), AnyState::KobIfdAsk(s), v, daa)
}

pub fn funding(amount: u64) -> KeyUtxo {
    KeyUtxo { utxo: utxo(amount, 500, None), pubkey: pk(MATCHER) }
}

pub fn signer() -> LocalKeys {
    LocalKeys { operator: pk(MATCHER), keys: (1..=40u8).map(|n| (pk(n), sk(n))).collect() }
}

pub fn input(book: &MemoryBook) -> TickInput {
    TickInput {
        orders: book.orders.clone(),
        clock: Clock { daa: book.daa_score, utc: UTC },
        funding: vec![funding(50 * KAS)],
        excluded: BTreeSet::new(),
    }
}

pub fn cfg() -> EngineConfig {
    EngineConfig::default()
}

/// The compute-budget table is exact over every generated shape and batch context: the executor's
/// one-unit slack retry is a safety net that never triggers in the tests.
pub fn assert_no_budget_slack() {
    assert_eq!(kob_executor::matcher::lower::budget_slack_retries(), 0, "a compute budget of the table was one unit short");
}

/// Runs a tick; every prepared transaction must have passed the engine.
pub fn run(inp: &TickInput, cfg: &EngineConfig) -> TickReport {
    let r = tick(inp, cfg, &Families::default(), &signer());
    assert_eq!(r.slack_retries, 0, "a compute budget of the table was one unit short (per-tick count)");
    if let Some((k, step, why)) = r.anomalies.first() {
        panic!("anomaly on book {} step {step}: {why}", kob_protocol::json::to_hex(&k.token));
    }
    for (k, why) in &r.skipped {
        eprintln!("skipped {}: {why}", kob_protocol::json::to_hex(&k.token));
    }
    for p in &r.prepared {
        assert!(p.validation.is_some() || !cfg.validate, "unvalidated transaction");
        check_invariants(p, inp);
    }
    if r.skipped.is_empty() && std::env::var_os("KOB_NO_KRON_TWIN").is_none() {
        run_kron_twin(inp, cfg);
    }
    r
}

/// The same book in the KRON family: the other family's planner path, builders and engine. Every scenario
/// of the suites runs in both families; the twin must plan without anomalies, and everything it builds
/// is engine-validated and satisfies the invariants (its own slot limits, 4 token inputs / 5 outputs, and
/// its own fees, may make it plan differently, so plans are not compared).
/// A KCC-20 token UTXO as the KRON token UTXO of the same holder.
pub fn kron_token(t: &TokenUtxo) -> TokenUtxo {
    let state = match &t.state {
        TokenState::Kcc20(k) if k.owner_scheme == SCHEME_COVID => KronState::custody(k.amount, k.owner).into(),
        TokenState::Kcc20(k) => KronState::addr(k.amount, k.owner).into(),
        s => s.clone(),
    };
    let mut utxo = t.utxo.clone();
    utxo.covenant_id = Some(TOKEN_COV_KRON);
    TokenUtxo { utxo, state }
}

/// An order as the same order of the KRON family.
pub fn kron_order(o: &ListedOrder) -> ListedOrder {
    ListedOrder {
        family: Family::Kron,
        order: OrderUtxo { utxo: o.order.utxo.clone(), state: kron_state(o.order.state.clone()) },
        custody: o.custody.as_ref().map(kron_token),
        foreign: vec![],
        strays: o.strays.iter().map(kron_token).collect(),
        ..o.clone()
    }
}

pub fn to_kron(inp: &TickInput) -> TickInput {
    let tok = |t: &TokenUtxo| -> TokenUtxo {
        let state = match &t.state {
            TokenState::Kcc20(k) if k.owner_scheme == SCHEME_COVID => KronState::custody(k.amount, k.owner).into(),
            TokenState::Kcc20(k) => KronState::addr(k.amount, k.owner).into(),
            s => s.clone(),
        };
        let mut utxo = t.utxo.clone();
        utxo.covenant_id = Some(TOKEN_COV_KRON);
        TokenUtxo { utxo, state }
    };
    TickInput {
        orders: inp
            .orders
            .iter()
            .map(|o| ListedOrder {
                family: Family::Kron,
                order: OrderUtxo { utxo: o.order.utxo.clone(), state: kron_state(o.order.state.clone()) },
                custody: o.custody.as_ref().map(&tok),
                foreign: vec![],
                strays: o.strays.iter().map(&tok).collect(),
                ..o.clone()
            })
            .collect(),
        clock: inp.clock,
        funding: inp.funding.clone(),
        excluded: inp.excluded.clone(),
    }
}

fn run_kron_twin(inp: &TickInput, cfg: &EngineConfig) {
    let k = to_kron(inp);
    let r = tick(&k, cfg, &Families::default(), &signer());
    if let Some((_, step, why)) = r.anomalies.first() {
        panic!("KRON twin: anomaly on step {step}: {why}");
    }
    assert!(r.skipped.is_empty(), "KRON twin skipped a book: {:?}", r.skipped.iter().map(|s| &s.1).collect::<Vec<_>>());
    for p in &r.prepared {
        assert!(p.validation.is_some() || !cfg.validate, "KRON twin: unvalidated transaction");
        check_invariants(p, &k);
    }
}

pub fn book(orders: Vec<ListedOrder>) -> MemoryBook {
    MemoryBook { daa_score: NOW + 5, orders, wallet_tokens: vec![] }
}

/// Invariants of every prepared transaction (matcher.md §8).
pub fn check_invariants(p: &Prepared, inp: &TickInput) {
    let tx = &p.signed.tx;
    // One input per outpoint and per covenant id (orders and merged entries).
    let mut ops = BTreeSet::new();
    for i in &tx.inputs {
        assert!(ops.insert((i.transaction_id, i.index)), "an outpoint spent twice");
    }
    let order_ids: Vec<[u8; 32]> =
        tx.inputs.iter().filter_map(|i| i.utxo.covenant_id).filter(|c| *c != TOKEN && *c != TOKEN_COV_KRON).collect();
    let set: BTreeSet<_> = order_ids.iter().collect();
    assert_eq!(set.len(), order_ids.len(), "one covenant input per order id");
    // No stray is ever spent.
    for o in &inp.orders {
        for s in &o.strays {
            assert!(!ops.contains(&(s.utxo.transaction_id, s.utxo.index)), "a stray was spent");
        }
    }
    // At most 8 token inputs (KRON: 4), and every KRON token output within 1..=1e9
    let fam = p.book.family;
    let tokc = if fam == Family::Kron { TOKEN_COV_KRON } else { TOKEN };
    let tok_in = tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(tokc)).count();
    assert!(tok_in <= fam.max_tok_in(), "{tok_in} token inputs");
    // No FOK partially filled; every fill within amountLeft.
    for f in &p.plan.fills {
        let o = inp.orders.iter().find(|o| o.id() == f.cand.id);
        if let Some(o) = o {
            match &o.base_state() {
                AnyState::KobAsk(s) if s.tif == TIF_FOK => assert_eq!(f.amount, s.amount_left, "FOK ask partially filled"),
                AnyState::KobBid(s) if s.tif == TIF_FOK => {
                    let (lo, hi) = f.cand.fok.expect("FOK range");
                    assert!(f.amount >= lo && f.amount <= hi, "FOK bid partially filled");
                    let left = o.order.utxo.amount as i64 - s.used(f.amount).expect("budget");
                    assert!(!s.can_continue(left), "FOK bid could buy more");
                }
                _ => {}
            }
            if let Some(l) = o.order.state.amount_left() {
                assert!(f.amount <= l);
            }
            // the minimum fill: below it only when the fill takes everything left (a bid: when it ends the bid)
            match &f.cand.pair {
                None => assert!(f.cand.quantity_ok(f.amount), "a fill of {} breaks the order's quantity rules", f.amount),
                Some(x) => assert!(x.info.quantity_ok(f.amount), "a pair fill of {} breaks its quantity rules", f.amount),
            }
        }
    }
    // Every order's positional payout is at least its all-in bound: the engine checked the
    // covenants; the operator's profit is what the plan promised (or better).
    assert!(p.accounting.profit >= 1, "unprofitable batch submitted: {}", p.accounting.profit);
    check_triggers(p, inp);
}

/// Base units of order `id` in a report's first transaction of its book.
pub fn amount_of(r: &TickReport, id: [u8; 32]) -> i64 {
    r.prepared.iter().filter(|p| p.step == 0).map(|p| p.plan.amount_of(&id)).sum()
}

/// Base units of order `id` over all steps.
pub fn amount_all(r: &TickReport, id: [u8; 32]) -> i64 {
    let mut ids: BTreeMap<[u8; 32], i64> = BTreeMap::new();
    for p in &r.prepared {
        for f in &p.plan.fills {
            *ids.entry(f.cand.id).or_default() += f.amount;
        }
    }
    ids.get(&id).copied().unwrap_or(0)
}

/// An order placed just now (IOC / FOK orders die 600 DAA after placement).
pub fn fresh(mut o: ListedOrder) -> ListedOrder {
    o.order.utxo.block_daa_score = NOW - 50;
    o.seen_daa = NOW - 50;
    if let Some(c) = &mut o.custody {
        c.utxo.block_daa_score = NOW - 50;
    }
    o
}

/// The batch request a prepared transaction was built from.
pub fn batch_request(p: &Prepared) -> kob_protocol::build::Batch {
    match serde_json::from_value::<kob_protocol::build::Action>(p.lowered.request.clone()).expect("a builder request") {
        kob_protocol::build::Action::Batch(b) => b,
        _ => panic!("the matcher lowers batches"),
    }
}

/// Every triggered fill and every update of a prepared batch reads a plain resting fill of the same batch that
/// qualifies by the library's own rules (`kob_protocol::build::touch_of`, `Touch::check`: the covenants' `touch`),
/// on the trigger side and at or through the stop (arms) or justifying a step (trailing ratchets); the order was
/// unarmed; an updated order is not also filled.
pub fn check_triggers(p: &Prepared, _inp: &TickInput) {
    use kob_protocol::build::{touch_of, Leg, Touch};
    let b = batch_request(p);
    let lock = b.lock_time as i64;
    let ev = |k: usize| -> Touch { touch_of(&b.legs[k]).unwrap_or_else(|e| panic!("evidence leg {k}: {e}")) };
    let arms = |t: &Touch, sell: bool, stop: i64| {
        if sell {
            t.side == SIDE_ASK && t.price <= stop
        } else {
            t.side == SIDE_BID && t.price >= stop
        }
    };
    let mut triggered = 0;
    // pair stops: the evidence (mode 0: a KAS-book leg of A and one of B; mode 1: a resting KobPair leg of the pair) arms the
    // order by its own rule
    let pair_ev = |e: usize, eb: Option<usize>| -> PairEvidence {
        match eb {
            Some(kb) => PairEvidence::KasBooks { a: ev(e).price, b: ev(kb).price },
            None => match &b.legs[e] {
                Leg::Pair { order, .. } => PairEvidence::Pair { price: order.state.price },
                l => panic!("pair evidence leg {e} is not a KobPair: {l:?}"),
            },
        }
    };
    for (i, l) in b.legs.iter().enumerate() {
        let (state, e, eb) = match l {
            Leg::CondPair { order, evidence: Some(e), evidence_b, .. } => {
                (AnyState::KobCondPair(order.state.clone()), *e, *evidence_b)
            }
            Leg::IfdPair { order, evidence: Some(e), evidence_b, .. } => (AnyState::KobIfdPair(order.state.clone()), *e, *evidence_b),
            _ => continue,
        };
        triggered += 1;
        assert_eq!(p.plan.fills[i].evidence, Some(e));
        assert_eq!(p.plan.fills[i].evidence_b, eb);
        let x = pair_ev(e, eb);
        let ok = match &state {
            AnyState::KobCondPair(s) => s.armed == 0 && s.arms(x) == Some(true),
            AnyState::KobIfdPair(s) => s.armed == 0 && s.arms(x) == Some(true),
            _ => false,
        };
        assert!(ok, "leg {i}: pair evidence {x:?} does not arm the stop");
    }
    for (i, l) in b.legs.iter().enumerate() {
        let (armed, stop, sell, token, scale, min_touch, min_rest, e) = match l {
            Leg::CondAsk { order, evidence: Some(e), .. } => {
                let s = &order.state;
                (s.armed, s.stop_price, true, s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, *e)
            }
            Leg::CondBid { order, evidence: Some(e), .. } => {
                let s = &order.state;
                (s.armed, s.stop_price, false, s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, *e)
            }
            Leg::IfdBid { order, evidence: Some(e), .. } => {
                let s = &order.state;
                (s.armed, s.entry_stop, false, s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, *e)
            }
            Leg::IfdAsk { order, evidence: Some(e), .. } => {
                let s = &order.state;
                (s.armed, s.entry_stop, true, s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, *e)
            }
            _ => continue,
        };
        triggered += 1;
        assert_eq!(armed, 0, "leg {i}: only an unarmed stop is triggered");
        assert_eq!(p.plan.fills[i].evidence, Some(e));
        let t = ev(e);
        t.check(token, scale, min_touch, min_rest, lock).unwrap_or_else(|x| panic!("leg {i}: {x}"));
        assert!(arms(&t, sell, stop), "leg {i}: evidence {t:?} does not trigger the stop {stop}");
    }
    assert_eq!(triggered, p.plan.triggered(), "every triggered fill carries its evidence");
    assert_eq!(b.updates.len(), p.plan.updates.len());
    for (u, pu) in b.updates.iter().zip(&p.plan.updates) {
        let id = u.order.utxo.covenant_id.expect("order");
        assert_eq!(id, pu.id);
        assert!(p.plan.fills.iter().all(|f| f.cand.id != id && f.cand.merge != Some(id)), "an updated order is not filled");
        if u.order.state.is_pair() {
            let x = pair_ev(u.evidence, u.evidence_b);
            let what = match &u.order.state {
                AnyState::KobCondPair(s) if s.arms(x) == Some(true) => "arm",
                AnyState::KobCondPair(s) if s.trail_k(x).is_some() => "trail",
                AnyState::KobIfdPair(s) if s.arms(x) == Some(true) => "arm",
                _ => "not justified",
            };
            assert_eq!(what, pu.kind.name(), "update of a pair order by {x:?}");
            continue;
        }
        let t = ev(u.evidence);
        let base = u.order.state.clone().into_family(Family::Kcc20);
        let (ok, what) = match &base {
            AnyState::KobCondAsk(s) => {
                t.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock).expect("evidence qualifies");
                assert_eq!(s.armed, 0);
                if arms(&t, true, s.stop_price) {
                    (true, "arm")
                } else {
                    (t.side == SIDE_BID && s.trail_steps(t.price) >= 1, "trail")
                }
            }
            AnyState::KobCondBid(s) => {
                t.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock).expect("evidence qualifies");
                assert_eq!(s.armed, 0);
                if arms(&t, false, s.stop_price) {
                    (true, "arm")
                } else {
                    (t.side == SIDE_ASK && s.trail_steps(t.price) >= 1, "trail")
                }
            }
            AnyState::KobIfdBid(s) => {
                t.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock).expect("evidence qualifies");
                (s.armed == 0 && arms(&t, false, s.entry_stop), "arm")
            }
            AnyState::KobIfdAsk(s) => {
                t.check(s.token_cov_id, s.scale, s.min_touch, s.min_rest_daa, lock).expect("evidence qualifies");
                (s.armed == 0 && arms(&t, true, s.entry_stop), "arm")
            }
            _ => (false, "not updatable"),
        };
        assert!(ok, "update of {}: {what} not justified by {t:?}", kob_protocol::json::to_hex(&id));
        assert_eq!(what, pu.kind.name());
    }
}
