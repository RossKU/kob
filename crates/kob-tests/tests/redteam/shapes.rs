//! The honest shapes the red-team suites attack: every builder shape of the tx dump (`kob-protocol/tests/tx_dump.rs`:
//! the golden-vector scenarios on every token program, with the delivery-mode scenarios on the KCC-20 programs, and the
//! pair scenarios), the pair delivery grid, the claim shapes (CL1 to CL3 of a bought-custody), IOC ends of a mode-1 bid
//! holding its custody, and the router intents (create and execute) of every actor on every program pair that runs it,
//! the 12 bought-custody (`_c`) actors included.
#![allow(dead_code)]

use crate::fx::pair::*;
use crate::fx::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::router::{Actor, IntentKind, IntentState, ACTORS};
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kob_protocol::Family;

/// The program pairs of the pair shapes (as `tx_dump.rs`).
pub const PAIRS: [(TemplateId, TemplateId); 8] = [
    (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20P2, TemplateId::Kcc20P2),
    (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20KaspaCom025),
    (TemplateId::Kcc20PublicMint, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
    (TemplateId::KronToken2433, TemplateId::KronToken2732),
];

pub type Shape = (String, BuiltTx);

fn built(v: Vec<(String, Action)>) -> Vec<Shape> {
    v.into_iter()
        .filter(|(n, _)| !n.contains("/grid.") && !n.starts_with("grid."))
        .filter_map(|(n, a)| build(&a).ok().map(|b| (n, b)))
        .collect()
}

/// The builder shapes on one token program (the tx dump's), at the default order carrier.
pub fn program_shapes(p: TemplateId) -> Vec<Shape> {
    with_carriers(Carriers::order_side(kob_protocol::defaults::DEFAULT_ORDER_CARRIER), || {
        let mut v: Vec<(String, Action)> = scenarios_on(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)).collect();
        if p.family() == Family::Kcc20 {
            v.extend(ioc_custody_scenarios(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)));
        }
        let all = v.len();
        let b = built(v);
        assert!(b.len() * 10 >= all * 9, "{}: {} of {all} scenarios build", p.name(), b.len());
        b
    })
}

/// The pair shapes (the tx dump's) and the pair delivery grid on one program pair.
pub fn pair_shapes(pa: TemplateId, pb: TemplateId) -> Vec<Shape> {
    with_carriers(Carriers::order_side(kob_protocol::defaults::DEFAULT_ORDER_CARRIER), || {
        let tag = pair_name(pa, pb);
        let mut v: Vec<(String, Action)> = pair_scenarios(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/{n}"), a)).collect();
        v.extend(pair_delivery_grid(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/dlv.{n}"), a)));
        built(v)
    })
}

/// A mode-1 bid holding its bought-custody, filled IOC / FOK: the fill ends the order and releases
/// the custody whole with the delivery.
pub fn ioc_custody_scenarios(t3: TemplateId) -> Vec<(String, Action)> {
    let bc = cov(0xb1);
    let held = || bid_dlv(Some(bought_custody(23, 4 * WHOLE, bc, 1_500)), None, None);
    let mut v = vec![];
    for (tif, name) in [(TIF_IOC, "ioc"), (TIF_FOK, "fok")] {
        let s = BidState { tif, ..mode_bid(bid(MAKER_B, P245, t3), MODE_ACCUMULATE, 4 * WHOLE) };
        let n = if tif == TIF_FOK { 6 } else { 3 };
        let value = s.escrow(6 * WHOLE, 1).unwrap() as u64;
        v.push((format!("take.bid.acc.{name}"), Action::Batch(take_bid_valued(s, n, value, held()))));
    }
    v
}

// ---------------------------------------------------------------- claims (bid-delivery-spec section 7)

const B_ID: [u8; 32] = [0xb1; 32];
const E_ID: [u8; 32] = [0xe1; 32];
const X_ID: [u8; 32] = [0x83; 32];

fn claim(order: OrderUtxo<AnyState>, custody: TokenUtxo, tokens: Vec<TokenUtxo>, place: Option<AnyState>, end: bool) -> ClaimOrder {
    let maker = order.state.maker();
    let key = (1..=40u8).find(|k| pk(*k) == maker).unwrap();
    ClaimOrder {
        order,
        bought_custody: custody,
        tokens,
        place: place.map(|o| Replacement { order: o, value: CARRIER, token_carrier: None, deadline: None }),
        end,
        value: None,
        token_carrier: None,
        funding: vec![key_utxo(30, key, 100 * KAS)],
        change: None,
        lock_time: 0,
        records: vec![],
        fee: fee(),
    }
}

/// CL1, CL2, CL3 (continuing and ending) of a `KobBid` and a `KobCondBid`, CL1 / CL2 of a `KobPair` and `KobCondPair`
/// ask, on every KCC-20 program.
pub fn claim_shapes() -> Vec<Shape> {
    let mut v = vec![];
    let kcc20: Vec<TemplateId> = PROGRAMS.into_iter().filter(|p| p.family() == Family::Kcc20).collect();
    for p in kcc20 {
        let n = p.name();
        let a4 = mode_bid(bid(MAKER_B, P245, p), MODE_ACCUMULATE, 4 * WHOLE);
        let o = order(20, a4.escrow(10 * WHOLE, 1).unwrap() as u64, B_ID, 1_000, AnyState::KobBid(a4));
        let c = bought_custody(23, 4 * WHOLE, B_ID, 1_500);
        let wallet = |tag: u8| tok(tag, WHOLE, pk(MAKER_B), SCHEME_P2PK, 1_000);
        let max_in = p.token_slots().unwrap().0.min(Family::Kcc20.max_tok_in());
        let mut add = |name: String, r: ClaimOrder| {
            if let Ok(b) = build_claim_order(&r, &kob_protocol::budget::lookup) {
                v.push((name, b));
            }
        };
        add(format!("{n}/claim.bid.CL1"), claim(o.clone(), c.clone(), vec![], None, false));
        let w: Vec<TokenUtxo> = (0..max_in.saturating_sub(1)).map(|k| wallet(40 + k as u8)).collect();
        add(format!("{n}/claim.bid.CL2"), claim(o.clone(), c.clone(), w, None, false));
        for end in [false, true] {
            let ask3 = AnyState::KobAsk(ask_n(MAKER_B, P250, p, 3));
            let nm = if end { "CL3end" } else { "CL3" };
            add(format!("{n}/claim.bid.{nm}"), claim(o.clone(), c.clone(), vec![wallet(41)], Some(ask3), end));
        }
        let c4 = CondBidState { amount_left: 6 * WHOLE, ..mode_cond_bid(cond_bid(MAKER_B, p), MODE_ACCUMULATE, 4 * WHOLE) };
        let co = order(40, c4.escrow(1).unwrap() as u64, E_ID, 2_000, AnyState::KobCondBid(c4));
        let cc = bought_custody(43, 4 * WHOLE, E_ID, 2_500);
        add(format!("{n}/claim.condBid.CL1"), claim(co.clone(), cc.clone(), vec![], None, false));
        add(format!("{n}/claim.condBid.CL3end"), claim(co, cc, vec![], Some(AnyState::KobAsk(ask_n(MAKER_B, P250, p, 4))), true));
        if p.serves_pairs() {
            let kp = PairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..pair(MAKER_A, true, p, p, RATE) };
            let kc = CondPairState { mode: MODE_ACCUMULATE, bought: 3 * WHOLE, ..cond_pair_ask(MAKER_A, p, p) };
            for (nm, st) in [("pair", AnyState::KobPair(kp)), ("condPair", AnyState::KobCondPair(kc))] {
                let o = order(70, pv(), X_ID, 1_000, st);
                let c = tutxo(p, TOKEN_B, 60, 3 * WHOLE, X_ID, true);
                for k in [0usize, 1] {
                    let w: Vec<TokenUtxo> = (0..k).map(|i| tutxo(p, TOKEN_B, 61 + i as u8, WHOLE, pk(MAKER_A), false)).collect();
                    add(format!("{n}/claim.{nm}.CL{}", k + 1), claim(o.clone(), c.clone(), w, None, false));
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------- router intents (as kob-protocol/tests/intent_builders.rs)

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const P3: TemplateId = TemplateId::Kcc20Ref;
const K1: TemplateId = TemplateId::KronToken2433;
const K2: TemplateId = TemplateId::KronToken2732;
const PM: TemplateId = TemplateId::Kcc20PublicMint;
const TOK_A: [u8; 32] = TOKEN_COV;
const TOK_B: [u8; 32] = TOKEN_B;
pub const PAYER: u8 = 11;
pub const KEEPER_K: u8 = 12;
const MCARRIER: u64 = KAS;
const FILLER: u64 = KAS / 5;
const DEADLINE: i64 = 1_800_000_000_000;

#[derive(Clone, Copy, Debug)]
pub struct Progs {
    pub a: TemplateId,
    pub b: TemplateId,
}
const RUNS: [Progs; 7] = [
    Progs { a: P8, b: P8 },
    Progs { a: P3, b: P3 },
    Progs { a: P3, b: P8 },
    Progs { a: K1, b: P8 },
    Progs { a: K2, b: P3 },
    Progs { a: PM, b: PM },
    Progs { a: K1, b: PM },
];

fn runs_for(actor: &Actor) -> Vec<Progs> {
    let mut out: Vec<Progs> = vec![];
    for p in RUNS {
        let a = actor.shape.kind.locks_tokens().then_some(p.a);
        let b = actor.shape.kind.merchant_gets_token().then_some(p.b);
        if a.is_some_and(|a| a.family() != actor.shape.a_family) {
            continue;
        }
        if out
            .iter()
            .any(|q| a == actor.shape.kind.locks_tokens().then_some(q.a) && b == actor.shape.kind.merchant_gets_token().then_some(q.b))
        {
            continue;
        }
        if actor.fits(a, b) {
            out.push(p);
        }
    }
    out
}

fn ask_leg(tag: u8, maker: u8, n: i64, take: i64, tif: i64, pb: TemplateId) -> IntentAsk {
    let c = cov(0x50 + tag);
    IntentAsk {
        order: order(tag, CARRIER, c, 1_000, AskState { token_cov_id: TOK_B, tif, ..ask_n(maker, P250, pb, n) }),
        custody: tok_of(tag + 100, n * WHOLE, c, SCHEME_COVID, 1_000, TOK_B),
        amount: take * WHOLE,
    }
}

fn bid_leg_mode(tag: u8, maker: u8, n: i64, ends: bool, pa: TemplateId, mode: i64, bought: i64) -> IntentBid {
    let s = BidState { mode, bought: bought * WHOLE, recv_threshold: i64::from(mode == 2), tif: 0, ..bid(maker, P245, pa) };
    let has_custody = mode == 1 && bought > 0;
    let unreserved = if has_custody { s.delivery_carrier } else { 0 };
    let value = if ends {
        s.escrow(n * WHOLE, 1).unwrap() - unreserved + s.used(WHOLE).unwrap() / 2
    } else {
        s.escrow((n + 5) * WHOLE, 1).unwrap()
    };
    let id = cov(0x60 + tag);
    let custody = has_custody.then(|| tok_of(tag + 120, bought * WHOLE, id, SCHEME_COVID, 1_000, TOK_A));
    IntentBid { order: order(tag, value as u64, id, 1_000, s), amount: n * WHOLE, custody }
}

fn leg_mode(actor: &Actor, i: usize, pa: TemplateId) -> (i64, i64) {
    if pa.family() == Family::Kron {
        (0, 0)
    } else if i < actor.custody {
        (1, 1)
    } else {
        (((i + 1) % 3) as i64, 0)
    }
}

fn legs_for(actor: &Actor, p: Progs) -> (Vec<IntentAsk>, Vec<IntentBid>) {
    let s = actor.shape;
    let asks = (0..s.asks)
        .map(|i| {
            let last = i + 1 == s.asks;
            if last && s.last_ask_rests {
                ask_leg(10 + i as u8, 20 + i as u8, 10, 2, TIF_GTC, p.b)
            } else {
                ask_leg(10 + i as u8, 20 + i as u8, 2, 2, if i % 2 == 0 { TIF_GTC } else { TIF_IOC }, p.b)
            }
        })
        .collect();
    let bids = (0..s.bids)
        .map(|i| {
            let last = i + 1 == s.bids;
            let (mode, bought) = leg_mode(actor, i, p.a);
            bid_leg_mode(30 + i as u8, 25 + i as u8, 2, !(last && s.last_bid_rests), p.a, mode, bought)
        })
        .collect();
    (asks, bids)
}

fn state_for(actor: &Actor, p: Progs, asks: &[IntentAsk], bids: &[IntentBid]) -> (IntentState, u64, i64, u64) {
    let bought: i64 = asks.iter().map(|a| a.amount).sum();
    let quote: i64 = asks.iter().map(|a| quote_of(a.amount, a.order.state.price, a.order.state.scale, Round::Up).unwrap()).sum();
    let sold: i64 = bids.iter().map(|b| b.amount).sum();
    let released: i64 = bids.iter().map(|b| b.order.state.spend(b.amount, 0, 1_000).unwrap()).sum();
    let lock = sold + i64::from(p.a.family() == Family::Kron);
    let (payer, merchant) = (pk(PAYER), pk(MERCHANT));
    match actor.shape.kind {
        IntentKind::KasToToken => {
            let max_extra = (MCARRIER + 4 * FILLER + KAS / 2) as i64;
            (
                IntentState::KasToToken {
                    payer,
                    merchant,
                    token: TOK_B,
                    program: p.b,
                    amount: bought,
                    max_pay: quote,
                    max_extra,
                    b_extension: EXT,
                    deadline: DEADLINE,
                },
                (quote + max_extra) as u64 + 5 * KAS,
                0,
                0,
            )
        }
        IntentKind::TokenToKas => {
            let merchant_kas = (released - KAS as i64 / 2) as u64;
            (
                IntentState::TokenToKas {
                    payer,
                    merchant,
                    token: TOK_A,
                    program: p.a,
                    merchant_kas: merchant_kas as i64,
                    max_sell: sold,
                    lock_amount: lock,
                    lock_extension: ext_for(p.a),
                    deadline: DEADLINE,
                },
                KAS,
                lock,
                merchant_kas,
            )
        }
        IntentKind::TokenSwap => (
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a: TOK_A,
                program_a: p.a,
                token_b: TOK_B,
                program_b: p.b,
                max_sell_a: sold,
                amount_b: bought,
                lock_amount: lock,
                lock_extension: ext_for(p.a),
                b_extension: EXT,
                deadline: DEADLINE,
            },
            (2 + actor.custody as u64) * KAS,
            lock,
            0,
        ),
    }
}

/// Router intents: for every actor and program pair that runs it, the creation (payer-signed) and the execution.
pub fn router_shapes() -> Vec<Shape> {
    let mut v = vec![];
    for actor in &ACTORS {
        for p in runs_for(actor) {
            let (asks, bids) = legs_for(actor, p);
            let (state, value, lock, merchant_kas) = state_for(actor, p, &asks, &bids);
            let tokens =
                if actor.shape.kind.locks_tokens() { vec![tok_on(p.a, 90, lock + 3 * WHOLE, pk(PAYER), 900)] } else { vec![] };
            let cr = CreateIntent {
                actor: actor.name.into(),
                state: state.clone(),
                value,
                tokens,
                lock_amount: lock,
                lock_carrier: KAS,
                token_change_carrier: KAS,
                funding: vec![key_utxo(91, PAYER, 200 * KAS)],
                change: None,
                records: vec![kob_protocol::payload::Record::X402 { reference: vec![0xab; 32] }],
                fee: fee(),
            };
            let Ok(created) = build_create_intent(&cr, &intent_budgets) else { continue };
            let tag = format!("router/{}/{}-{}", actor.name, p.a.name(), p.b.name());
            let id = created.covenants[0].covenant_id;
            let sigs = sign_locally(&created, &keys()).expect("sign");
            let s = finalize(&created, &sigs, FinalizeOptions { tighten_budgets: true }).expect("finalize");
            let (tx, _) = s.tx.to_tx().unwrap();
            let txid = tx.id().as_bytes();
            let intent =
                Utxo { transaction_id: txid, index: INTENT_OUTPUT, amount: value, block_daa_score: 2_000, covenant_id: Some(id) };
            let lock_utxo = actor.shape.kind.locks_tokens().then(|| {
                let o = &tx.outputs[LOCK_OUTPUT as usize];
                TokenUtxo {
                    utxo: Utxo {
                        transaction_id: txid,
                        index: LOCK_OUTPUT,
                        amount: o.value,
                        block_daa_score: 2_000,
                        covenant_id: Some(TOK_A),
                    },
                    state: TokenState::custody(p.a.family(), lock, id, ext_for(p.a)),
                }
            });
            v.push((format!("{tag}/create"), created));
            let r = ExecuteIntent {
                actor: actor.name.into(),
                state,
                intent,
                lock: lock_utxo,
                asks,
                bids,
                merchant_carrier: MCARRIER,
                merchant_kas,
                payer_token_carrier: None,
                filler: FILLER,
                keeper: pk(KEEPER_K),
                lock_time: NOW,
                records: vec![],
                fee: fee(),
            };
            if let Ok((b, _)) = build_execute_intent(&r, &intent_budgets) {
                v.push((format!("{tag}/execute"), b));
            }
        }
    }
    v
}
