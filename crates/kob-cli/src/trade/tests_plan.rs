//! The CLI planner against the web wallet's planner: the same fixtures as `web/src/testing/fixtures.ts` (maker key 11..11,
//! token 70..70 / extension ee..ee, 3 decimals, tick 100, DAA 1 000 000 at 15:00 UTC, 1 000 KAS of funding, 100 tokens, a
//! 10 KAS fixture carrier, the default book 2.50 / 2.45 KAS) and the numbers the web planner tests assert for the same
//! intents (`web/src/kob/orders/simple.test.ts`, `cond.test.ts`, `cond-ifd.test.ts`, `pair-simple.test.ts`), plus the
//! golden vectors of `crates/kob-protocol/vectors/golden.json` the web tests compare their plans with (market orders,
//! `create.bid`, `pair.create.ask` / `pair.create.bid`). Every plan is signed with the maker key, finalized, validated in the
//! script engine and its placement record recovered from the transaction.

use std::collections::BTreeMap;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::payload::recover_orders;
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{finalize, pubkey_of, sign_locally, FinalizeOptions, KeyUtxo, TokenUtxo, Utxo};
use kob_protocol::verify::validate_signed;
use serde_json::Value;

use super::market::TokenMarket;
use super::plan::*;

pub(super) const KAS: i64 = 100_000_000;
pub(super) const TOK: i64 = 1000;
pub(super) const C: i64 = 1_000_000_000;
pub(super) const GTC: i64 = 1_000_000 + MAX_IDLE_DAA;
pub(super) const MAKER_SK: [u8; 32] = [0x11; 32];

pub(super) fn maker() -> [u8; 32] {
    pubkey_of(&MAKER_SK).unwrap()
}

pub(super) fn market(p: TemplateId) -> TokenMarket {
    let ext = (p.family() == kob_protocol::Family::Kcc20).then_some([0xee; 32]);
    TokenMarket::new([0x70; 32], "TST", 3, p, ext, Some(100)).unwrap()
}

pub(super) fn markets() -> Vec<TokenMarket> {
    vec![market(TemplateId::Kcc20Ref), market(TemplateId::Kcc20Ref8x8), market(TemplateId::KronToken2433)]
}

pub(super) fn txid(n: u8) -> [u8; 32] {
    let mut t = [0u8; 32];
    t[31] = n;
    t
}

pub(super) fn key_utxo(n: u8, amount: i64) -> KeyUtxo {
    KeyUtxo {
        utxo: Utxo { transaction_id: txid(n), index: 0, amount: amount as u64, block_daa_score: 500, covenant_id: None },
        pubkey: maker(),
    }
}

pub(super) fn token_utxo(m: &TokenMarket, n: u8, amount: i64) -> TokenUtxo {
    TokenUtxo {
        utxo: Utxo { transaction_id: txid(n), index: 1, amount: 10 * KAS as u64, block_daa_score: 500, covenant_id: Some(m.cov) },
        state: TokenState::user(m.family, amount, maker(), m.ext),
    }
}

pub(super) fn level(price: i64, amount: i64, orders: i64) -> Level {
    Level { price, amount, orders, tip: 0 }
}

pub(super) fn default_book() -> Book {
    Book {
        asks: vec![level(250_000_000, 5 * TOK, 2), level(252_000_000, 10 * TOK, 3), level(260_000_000, 20 * TOK, 4)],
        bids: vec![level(245_000_000, 5 * TOK, 1), level(243_000_000, 10 * TOK, 2), level(240_000_000, 20 * TOK, 3)],
        ask_orders: None,
        bid_orders: None,
    }
}

pub(super) const CLOCK: Clock = Clock { daa: 1_000_000, unix: 1_790_694_000, rate_milli: 10_000 };

pub(super) fn env(m: &TokenMarket) -> Env {
    Env {
        market: m.clone(),
        maker: maker(),
        clock: CLOCK,
        book: default_book(),
        own: vec![],
        guards_unavailable: vec![],
        guards_acknowledged: false,
        last_fill: None,
        market_start_ack: true,
        funding: vec![key_utxo(1, 1000 * KAS)],
        token_utxos: vec![token_utxo(m, 2, 100 * TOK)],
        fee_rate: None,
        carrier: Some(C as u64),
        pair: None,
    }
}

/// Plans, expects success, signs, finalizes, validates in the engine and recovers the placement record.
pub(super) fn valid(e: &Env, i: Intent) -> Plan {
    let p = plan_order(e, &i);
    let errs: Vec<&Issue> = p.issues.iter().filter(|x| x.severity == Severity::Error).collect();
    assert!(errs.is_empty() && p.ok, "{errs:?}");
    let built = p.built.clone().unwrap();
    let keys = BTreeMap::from([(maker(), MAKER_SK)]);
    let sigs = sign_locally(&built, &keys).unwrap();
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).unwrap();
    validate_signed(&signed).unwrap();
    let rec = recover_orders(&built.tx).unwrap();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].order, p.states[0]);
    assert_eq!(rec[0].value, p.request.as_ref().unwrap().value);
    let ins: u64 = built.tx.inputs.iter().map(|x| x.utxo.amount).sum();
    let outs: u64 = built.tx.outputs.iter().map(|x| x.value).sum();
    assert_eq!(ins, outs + built.fee.fee);
    p
}

pub(super) fn codes(p: &Plan) -> Vec<&'static str> {
    p.issues.iter().map(|i| i.code).collect()
}

pub(super) fn ask(p: &Plan) -> kob_protocol::state::AskState {
    match &p.states[0] {
        AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.clone(),
        o => panic!("not an ask: {o:?}"),
    }
}
pub(super) fn bid(p: &Plan) -> kob_protocol::state::BidState {
    match &p.states[0] {
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.clone(),
        o => panic!("not a bid: {o:?}"),
    }
}

pub(super) fn limit(side: Side, amount: i64, price: i64) -> LimitIntent {
    LimitIntent {
        side,
        amount,
        price,
        tip: 0,
        min_fill: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        crossing: Crossing::Auction,
        max_fills: None,
    }
}

pub(super) fn golden(name: &str) -> (AnyState, u64) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../kob-protocol/vectors/golden.json");
    let g: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let t = g["transactions"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap();
    let st: AnyState = serde_json::from_value(t["request"]["order"].clone()).unwrap();
    (st, t["request"]["value"].as_str().unwrap().parse().unwrap())
}

#[test]
fn gtc_sell_on_every_program() {
    for m in markets() {
        let p = valid(&env(&m), Intent::Limit(LimitIntent { tip: 100_000, ..limit(Side::Sell, 10 * TOK, 300_000_000) }));
        let s = ask(&p);
        assert_eq!(
            (s.maker, s.token_cov_id, s.token_tpl_hash, s.scale, s.price, s.tip, s.tif),
            (maker(), m.cov, m.tpl_hash, 1000, 300_000_000, 100_000, 0)
        );
        assert_eq!((s.active_from, s.expiry_daa, s.refund_tip, s.amount_left, s.decay_step), (0, GTC, m.refund_tip, 10_000, 1));
        // the amount worth 10 KAS at 3 KAS per token
        assert_eq!(s.min_fill, 3334);
        let d = p.disclosure.unwrap();
        assert_eq!((d.limit_price, d.all_in_price, d.all_in_total), (Some(300_000_000), Some(299_900_000), Some(2_999_000_000)));
        assert_eq!((d.tokens_escrowed, d.kas_locked, d.expiry_kind), (10_000, 20 * KAS as i128, "gtc"));
        assert_eq!(d.expiry_unix, CLOCK.unix + 90 * 86_400);
        let kinds: Vec<(&str, bool)> = d.carriers.iter().map(|c| (c.kind, c.kept)).collect();
        assert_eq!(kinds, vec![("order carrier", false), ("custody carrier", false), ("token change carrier", true)]);
        assert_eq!(p.built.unwrap().covenants.len(), 1);
    }
}

#[test]
fn gtc_buy_is_the_golden_create_bid_escrow() {
    let m = market(TemplateId::Kcc20Ref);
    let p = valid(&env(&m), Intent::Limit(LimitIntent { tip: 100_000, ..limit(Side::Buy, 10 * TOK, 245_000_000) }));
    let s = bid(&p);
    assert_eq!((s.min_fill, s.delivery_carrier, s.reserve, s.refund_tip), (4082, C, 0, 3_500_000));
    // used(10000) + 2 sompi of rounding + 3 delivery carriers: the Rust golden vector `create.bid`
    let (g, gv) = golden("create.bid");
    assert_eq!(p.request.as_ref().unwrap().value, 5_451_000_002);
    assert_eq!(p.request.as_ref().unwrap().value, gv);
    // the golden state differs only in what the vector fixes differently (maker, expiry, min fill, decay step)
    if let AnyState::KobBid(g) = g {
        assert_eq!(
            (g.price, g.tip, g.token_cov_id, g.token_tpl_hash, g.extension_commitment),
            (s.price, s.tip, s.token_cov_id, s.token_tpl_hash, s.extension_commitment)
        );
    } else {
        panic!("golden create.bid is a bid");
    }
    let d = p.disclosure.unwrap();
    assert_eq!((d.all_in_price, d.all_in_total, d.kas_locked), (Some(245_100_000), Some(2_451_000_000), 5_451_000_002));
    assert_eq!(d.carriers[0].amount, 2_451_000_002);
    assert_eq!((d.carriers[1].amount, d.carriers[1].count), (C as i128, 3));
}

#[test]
fn defaults_fills_and_refusals_like_the_web_planner() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    let p = 250_000_000;
    let escrow = |amount: i64, rate: i64, fills: i64| (amount * rate + 999) / 1000 + (fills - 1) + fills * C;
    let v = |i: LimitIntent| valid(&e, Intent::Limit(i)).request.unwrap().value as i64;
    assert_eq!(v(limit(Side::Buy, 2 * TOK, p)), escrow(2 * TOK, p, 1));
    assert_eq!(v(LimitIntent { min_fill: Some(500), ..limit(Side::Buy, 2 * TOK, p) }), escrow(2 * TOK, p, 3));
    assert_eq!(v(LimitIntent { max_fills: Some(5), ..limit(Side::Buy, 10 * TOK, p) }), escrow(10 * TOK, p, 5));
    assert!(codes(&plan_order(&e, &Intent::Limit(LimitIntent { max_fills: Some(0), ..limit(Side::Buy, 10 * TOK, p) })))
        .contains(&"MAX_FILLS_INVALID"));
    assert!(codes(&plan_order(&e, &Intent::Limit(LimitIntent { max_fills: Some(2), ..limit(Side::Sell, 10 * TOK, p) })))
        .contains(&"SIDE_INVALID"));
    assert_eq!(ask(&valid(&e, Intent::Limit(limit(Side::Sell, 10 * TOK, p)))).min_fill, 4000);
    assert_eq!(ask(&valid(&e, Intent::Limit(limit(Side::Sell, 10 * TOK, 1_000_000)))).min_fill, 10_000);
    for mf in [0, 10 * TOK + 1] {
        assert!(codes(&plan_order(&e, &Intent::Limit(LimitIntent { min_fill: Some(mf), ..limit(Side::Sell, 10 * TOK, p) })))
            .contains(&"MIN_FILL_INVALID"));
    }
    // off the tick: refused with the nearest valid prices
    let off = plan_order(&e, &Intent::Limit(limit(Side::Sell, 10 * TOK, p + 50)));
    assert_eq!(codes(&off), vec!["PRICE_NOT_ON_TICK"]);
    // not enough tokens / KAS
    let poor = Env { token_utxos: vec![token_utxo(&m, 2, TOK)], ..e.clone() };
    assert!(codes(&plan_order(&poor, &Intent::Limit(limit(Side::Sell, 10 * TOK, p)))).contains(&"INSUFFICIENT_TOKENS"));
    let broke = Env { funding: vec![key_utxo(1, KAS)], ..e.clone() };
    assert!(codes(&plan_order(&broke, &Intent::Limit(limit(Side::Buy, 10 * TOK, p)))).contains(&"INSUFFICIENT_KAS"));
    // a guard that could not run blocks until acknowledged
    let blind = Env { guards_unavailable: vec!["book".into()], ..e.clone() };
    assert!(codes(&plan_order(&blind, &Intent::Limit(limit(Side::Sell, 10 * TOK, p)))).contains(&"GUARDS_UNAVAILABLE"));
    let acked = Env { guards_acknowledged: true, ..blind };
    assert!(plan_order(&acked, &Intent::Limit(limit(Side::Sell, 10 * TOK, p))).ok);
    // self-trade
    let own = OwnOrder { cov: [9; 32], side: Side::Buy, price: 260_000_000, tip: 0, amount_left: 1, active: true };
    let st = Env { own: vec![own], ..e.clone() };
    assert!(codes(&plan_order(&st, &Intent::Limit(limit(Side::Sell, 10 * TOK, 255_000_000)))).contains(&"SELF_TRADE"));
}

#[test]
fn gtd_day_and_timed_activation() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    let p = 250_000_000;
    for side in [Side::Sell, Side::Buy] {
        let g = valid(&e, Intent::Limit(LimitIntent { lifetime: Lifetime::GtdUnix(CLOCK.unix + 3600), ..limit(side, 3 * TOK, p) }));
        assert_eq!(g.states[0].expiry().map(|x| x.0), Some(CLOCK.daa + 36_000));
        let d = valid(&e, Intent::Limit(LimitIntent { lifetime: Lifetime::Day, ..limit(side, 3 * TOK, p) }));
        assert_eq!(d.states[0].expiry().map(|x| x.0), Some(1_327_240));
        assert_eq!(d.request.as_ref().unwrap().deadline, Some(1_790_726_400));
        assert_eq!(recover_orders(&d.built.unwrap().tx).unwrap()[0].deadline, Some(1_790_726_400));
        let a =
            valid(&e, Intent::Limit(LimitIntent { active_from: Some(Activation::Unix(CLOCK.unix + 600)), ..limit(side, 3 * TOK, p) }));
        assert_eq!(a.disclosure.unwrap().activates_at, Some((CLOCK.daa + 6_000, CLOCK.unix + 600)));
    }
    let far = plan_order(
        &e,
        &Intent::Limit(LimitIntent { lifetime: Lifetime::GtdUnix(CLOCK.unix + 91 * 86_400), ..limit(Side::Sell, 3 * TOK, p) }),
    );
    assert!(codes(&far).contains(&"EXPIRY_TOO_FAR"));
    let past = plan_order(
        &e,
        &Intent::Limit(LimitIntent { active_from: Some(Activation::Daa(CLOCK.daa - 5)), ..limit(Side::Sell, 3 * TOK, p) }),
    );
    assert!(past.ok && ask(&past).active_from == 0 && codes(&past).contains(&"ACTIVE_IN_PAST"));
}

#[test]
fn ioc_fok_and_the_crossing_policy() {
    for m in markets() {
        let e = env(&m);
        for side in [Side::Sell, Side::Buy] {
            let price = if side == Side::Sell { 245_000_000 } else { 250_000_000 };
            let p = valid(
                &e,
                Intent::Immediate(ImmediateIntent {
                    fok: false,
                    side,
                    amount: 4 * TOK,
                    price,
                    tip: 0,
                    min_fill: None,
                    life_daa: None,
                    active_from: None,
                }),
            );
            let (tif, exp, mf) = match &p.states[0] {
                AnyState::KobAsk(s) | AnyState::KobAskKron(s) => (s.tif, s.expiry_daa, s.min_fill),
                AnyState::KobBid(s) | AnyState::KobBidKron(s) => (s.tif, s.expiry_daa, s.min_fill),
                _ => unreachable!(),
            };
            assert_eq!((tif, exp, mf), (1, CLOCK.daa + 300, 1));
        }
        let fok = valid(
            &e,
            Intent::Immediate(ImmediateIntent {
                fok: true,
                side: Side::Sell,
                amount: 6 * TOK,
                price: 243_000_000,
                tip: 0,
                min_fill: None,
                life_daa: None,
                active_from: None,
            }),
        );
        assert_eq!(ask(&fok).tif, 2);
        let short = plan_order(
            &e,
            &Intent::Immediate(ImmediateIntent {
                fok: true,
                side: Side::Sell,
                amount: 6 * TOK,
                price: 245_000_000,
                tip: 0,
                min_fill: None,
                life_daa: None,
                active_from: None,
            }),
        );
        assert_eq!(codes(&short), vec!["FOK_INSUFFICIENT_DEPTH"]);
        // a sell limit below the best bid crosses: an auction from the touch down to the limit (matcher.md 10.5)
        let x = valid(&e, Intent::Limit(limit(Side::Sell, 3 * TOK, 240_000_000)));
        let s = ask(&x);
        assert_eq!((s.price, s.price_end, s.active_from, s.decay_step), (245_000_000, 240_000_000, CLOCK.daa + 30, 1));
        assert_eq!(s.slope, decay_slope(245_000_000, 240_000_000, 200, 1));
        assert!(codes(&x).contains(&"MARKETABLE_AUCTION"));
        let r = plan_order(&e, &Intent::Limit(LimitIntent { crossing: Crossing::Reject, ..limit(Side::Sell, 3 * TOK, 240_000_000) }));
        assert!(codes(&r).contains(&"MARKETABLE_REJECTED"));
    }
}

#[test]
fn market_orders_equal_the_golden_vectors() {
    for (prefix, p) in [("", TemplateId::Kcc20Ref), ("kron.", TemplateId::KronToken2433)] {
        let m = market(p);
        let sell_env = Env {
            clock: Clock { daa: 999_870, ..CLOCK },
            book: Book { bids: vec![level(260_000_000, 40 * TOK, 2)], ..Default::default() },
            ..env(&m)
        };
        let mk = |side, mf| {
            Intent::Market(MarketIntent {
                side,
                amount: if side == Side::Sell { 10 * TOK } else { 4 * TOK },
                tip: 100_000,
                min_fill: mf,
                slippage_bps: None,
                all_or_nothing: false,
            })
        };
        assert_eq!(ask(&valid(&sell_env, mk(Side::Sell, None))).min_fill, 1);
        let s = valid(&sell_env, mk(Side::Sell, Some(TOK)));
        let (g, _) = golden(&format!("{prefix}create.ask.market"));
        let mine = match (&s.states[0], g) {
            (AnyState::KobAsk(a), AnyState::KobAsk(g)) | (AnyState::KobAskKron(a), AnyState::KobAskKron(g)) => {
                (a.clone(), kob_protocol::state::AskState { maker: a.maker, ..g })
            }
            o => panic!("{o:?}"),
        };
        assert_eq!(mine.0, mine.1, "{prefix}create.ask.market");
        let d = s.disclosure.unwrap();
        assert_eq!((d.expected_price, d.worst_price, d.all_in_price), (Some(260_000_000), Some(252_200_000), Some(252_100_000)));
        let buy_env = Env {
            clock: Clock { daa: 999_870, ..CLOCK },
            book: Book { asks: vec![level(240_000_000, 40 * TOK, 2)], ..Default::default() },
            ..env(&m)
        };
        let b = valid(&buy_env, mk(Side::Buy, Some(TOK)));
        let (g, gv) = golden(&format!("{prefix}create.bid.market"));
        let mine = match (&b.states[0], g) {
            (AnyState::KobBid(a), AnyState::KobBid(g)) | (AnyState::KobBidKron(a), AnyState::KobBidKron(g)) => {
                (a.clone(), kob_protocol::state::BidState { maker: a.maker, ..g })
            }
            o => panic!("{o:?}"),
        };
        assert_eq!(mine.0, mine.1, "{prefix}create.bid.market");
        assert_eq!(b.request.unwrap().value, gv);
        assert_eq!(gv, 1_989_200_000);
    }
    // the start is held against the last fill unless acknowledged
    let m = market(TemplateId::Kcc20Ref);
    let e = Env { market_start_ack: false, last_fill: Some(200_000_000), ..env(&m) };
    let p = plan_order(
        &e,
        &Intent::Market(MarketIntent {
            side: Side::Buy,
            amount: 3 * TOK,
            tip: 0,
            min_fill: None,
            slippage_bps: None,
            all_or_nothing: false,
        }),
    );
    assert!(!p.ok && codes(&p).contains(&"MARKET_START_VS_LAST_FILL"));
}

pub(super) fn cond(kind: CondKind, side: Side, amount: i64) -> CondIntent {
    CondIntent {
        kind,
        side,
        amount,
        tip: 0,
        min_fill: None,
        stop: None,
        limit: None,
        take_profit: None,
        slip_bps: None,
        min_touch: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        max_fills: None,
        trail: None,
        knobs: StopKnobs::default(),
    }
}

#[test]
fn stops_take_profit_and_oco_like_the_web_planner() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    // stop-market sell: wallet defaults
    let p = valid(&e, Intent::Cond(CondIntent { stop: Some(230_000_000), ..cond(CondKind::StopMarket, Side::Sell, 10 * TOK) }));
    let AnyState::KobCondAsk(s) = &p.states[0] else { panic!() };
    assert_eq!(
        (s.min_fill, s.min_touch, s.refund_tip, s.keeper_tip, s.slip_bps, s.band_daa, s.min_rest_daa, s.expiry_daa),
        (4348, 4348, 3_500_000, 2_200_000, 300, 300, 50, GTC)
    );
    let d = p.disclosure.unwrap();
    assert_eq!(
        (d.limit_price, d.worst_price, d.expected_price, d.all_in_total, d.kas_locked),
        (Some(223_100_000), Some(223_100_000), Some(230_000_000), Some(2_231_000_000), 2 * C as i128)
    );
    // stop-market buy: escrow at the stop ceiling, three delivery carriers, the arming tip
    let p = valid(&e, Intent::Cond(CondIntent { stop: Some(260_000_000), ..cond(CondKind::StopMarket, Side::Buy, 10 * TOK) }));
    let ceiling = 260_000_000 + 26_000 * 300;
    assert_eq!(p.request.unwrap().value as i64, 10 * ceiling + 3 * C + 2_200_000);
    let AnyState::KobCondBid(s) = &p.states[0] else { panic!() };
    assert_eq!((s.min_fill, s.min_touch), (3847, 3847));
    // stop-limit: the largest band within the limit
    let p = valid(
        &e,
        Intent::Cond(CondIntent {
            stop: Some(230_000_000),
            limit: Some(225_000_000),
            ..cond(CondKind::StopLimit, Side::Sell, 10 * TOK)
        }),
    );
    let AnyState::KobCondAsk(s) = &p.states[0] else { panic!() };
    assert_eq!(s.slip_bps, 217);
    let p = valid(
        &e,
        Intent::Cond(CondIntent {
            stop: Some(260_000_000),
            limit: Some(268_000_000),
            ..cond(CondKind::StopLimit, Side::Buy, 10 * TOK)
        }),
    );
    let AnyState::KobCondBid(s) = &p.states[0] else { panic!() };
    assert_eq!(s.slip_bps, 307);
    // take-profit buy: the limit all-in for the amount plus delivery carriers, no keeper
    let p = valid(
        &e,
        Intent::Cond(CondIntent { take_profit: Some(200_000_000), tip: 20_000, ..cond(CondKind::TakeProfit, Side::Buy, 10 * TOK) }),
    );
    let AnyState::KobCondBid(s) = &p.states[0] else { panic!() };
    assert_eq!((s.tp_price, s.stop_price, s.keeper_tip, s.min_fill), (200_000_000, 0, 0, 5000));
    assert_eq!(p.request.as_ref().unwrap().value as i64, 10 * 200_020_000 + 2 * C);
    // OCO sell: one covenant with both legs
    let p = valid(
        &e,
        Intent::Cond(CondIntent {
            take_profit: Some(300_000_000),
            stop: Some(230_000_000),
            ..cond(CondKind::Oco, Side::Sell, 10 * TOK)
        }),
    );
    let AnyState::KobCondAsk(s) = &p.states[0] else { panic!() };
    assert_eq!((s.tp_price, s.stop_price, s.min_fill), (300_000_000, 230_000_000, 4348));
    let bad = plan_order(
        &e,
        &Intent::Cond(CondIntent {
            take_profit: Some(220_000_000),
            stop: Some(230_000_000),
            ..cond(CondKind::Oco, Side::Sell, 10 * TOK)
        }),
    );
    assert!(codes(&bad).contains(&"COND_TP_STOP_ORDER"));
    // a stop of a token whose genesis is not verified is a warning
    assert!(codes(&valid(&e, Intent::Cond(CondIntent { stop: Some(230_000_000), ..cond(CondKind::StopMarket, Side::Sell, TOK) })))
        .contains(&"STOP_EVIDENCE_GENESIS_UNVERIFIED"));
}

pub(super) fn ifd(side: Side, price: i64, tp: Option<i64>, stop: Option<i64>) -> IfdIntent {
    IfdIntent {
        side,
        amount: 10 * TOK,
        tip: 0,
        min_fill: None,
        price,
        take_profit: tp,
        stop,
        stop_limit: None,
        exit_tip: 0,
        prefund: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        more: IfdMore::default(),
    }
}

#[test]
fn if_done_entries_like_the_web_planner() {
    for m in markets() {
        let e = env(&m);
        // buy first: 4 fills, each a delivery and an exit carrier
        let p = valid(&e, Intent::Ifd(ifd(Side::Buy, 240_000_000, Some(300_000_000), None)));
        assert_eq!(p.states.len(), 2);
        let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &p.states[0] else { panic!() };
        assert_eq!(
            (s.min_fill, s.entry_stop, s.band_daa, s.keeper_tip, s.min_touch, s.min_rest_daa, s.expiry_daa),
            (2500, 0, 0, 0, 0, 50, GTC)
        );
        assert_eq!(p.request.as_ref().unwrap().value as i64, 10 * 240_000_000 + 8 * C);
        let (AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x)) = &p.states[1] else { panic!() };
        assert_eq!((x.tp_price, x.min_fill, x.expiry_daa, x.amount_left), (300_000_000, 2500, 1 << 62, 10_000));
        assert_eq!(s.exit().unwrap(), *x, "the committed exit round-trips");
        // sell first: custody, prefund rounding of 3 sompi
        let p = valid(&e, Intent::Ifd(ifd(Side::Sell, 260_000_000, Some(240_000_000), None)));
        let (AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s)) = &p.states[0] else { panic!() };
        assert_eq!((s.prefund, s.min_fill), (0, 2500));
        assert_eq!(p.request.as_ref().unwrap().value as i64, C + 4 * C + 3);
        let d = p.disclosure.unwrap();
        assert_eq!((d.all_in_total, d.kas_locked, d.tokens_escrowed), (Some(2_600_000_000), 6 * C as i128 + 3, 10_000));
        // IFO: an OCO exit
        let p = valid(&e, Intent::Ifd(ifd(Side::Buy, 240_000_000, Some(300_000_000), Some(200_000_000))));
        let (AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x)) = &p.states[1] else { panic!() };
        assert_eq!((x.tp_price, x.stop_price, x.slip_bps), (300_000_000, 200_000_000, 300));
    }
}

#[test]
fn pair_limits_equal_the_golden_vectors() {
    let a = market(TemplateId::Kcc20Ref);
    let b = TokenMarket { cov: [0x71; 32], ticker: "G71".into(), ..market(TemplateId::Kcc20Ref) };
    for (name, side) in [("pair.create.ask", Side::Sell), ("pair.create.bid", Side::Buy)] {
        let e = Env {
            book: Book::default(),
            carrier: Some(200_000_000),
            funding: vec![key_utxo(1, 100_000 * KAS)],
            token_utxos: vec![token_utxo(&a, 2, 100 * TOK)],
            pair: Some(PairPart {
                quote: b.clone(),
                quote_utxos: vec![token_utxo(&b, 3, 1_000_000 * 1000)],
                kas_per_whole_a: Some(3 * KAS),
                kas_per_whole_b: None,
            }),
            ..env(&a)
        };
        let p = valid(&e, Intent::Limit(LimitIntent { min_fill: Some(1_000), max_fills: Some(9), ..limit(side, 10_000, 1_000) }));
        let (AnyState::KobPair(g), gv) = golden(name) else { panic!() };
        let AnyState::KobPair(s) = &p.states[0] else { panic!() };
        // equal but for what the vector fixes differently: maker, expiry, decay step; the escrow is the floor plus one unit
        assert_eq!(
            *s,
            kob_protocol::state::PairState {
                maker: s.maker,
                expiry_daa: s.expiry_daa,
                decay_step: s.decay_step,
                custody: s.custody,
                ..g
            },
            "{name}"
        );
        assert_eq!(s.custody, if side == Side::Sell { 10_000 } else { 10_001 });
        assert_eq!(p.request.unwrap().value, gv, "{name}");
    }
}

/// A new order's carriers become outputs with a covenant id: the planner refuses a carrier below the KIP-9 covenant output
/// bound (0.08 KAS, `tx::COVENANT_OUTPUT_MIN`), the plain-output bound of 0.02 KAS included, before building (matcher.md
/// 10.1), on a sell's custody carrier and a buy's delivery carriers alike; KaspaCom's own floor (0.5 KAS) stays above it.
#[test]
fn new_order_carriers_clear_the_covenant_output_minimum() {
    use kob_protocol::tx::{COVENANT_OUTPUT_MIN, DUST_OUTPUT_MIN};
    let m = market(TemplateId::Kcc20Ref);
    assert_eq!(order_carrier_floor(&m), COVENANT_OUTPUT_MIN as i64);
    assert_eq!(token_carrier_floor(&m), DUST_OUTPUT_MIN as i64);
    assert_eq!(order_carrier_floor(&market(TemplateId::Kcc20KaspaCom025)), 50_000_000);
    for c in [DUST_OUTPUT_MIN, COVENANT_OUTPUT_MIN - 1] {
        let e = Env { carrier: Some(c), ..env(&m) };
        for side in [Side::Sell, Side::Buy] {
            let p = plan_order(&e, &Intent::Limit(LimitIntent { tip: 100_000, ..limit(side, 10 * TOK, 250_000_000) }));
            assert!(!p.ok && codes(&p).contains(&"CARRIER_BELOW_FLOOR"), "{c} {side:?}: {:?}", codes(&p));
        }
    }
}

/// A hostile indexer's numbers (prices and amounts at the i64 limits, sums that overflow) never panic the planner or the
/// book readers: they are refused or flagged like any other bad book.
#[test]
fn a_hostile_book_never_panics() {
    let m = market(TemplateId::Kcc20Ref);
    let big = Book {
        asks: vec![level(i64::MAX - 1, i64::MAX, 1), level(i64::MAX, i64::MAX, i64::MAX)],
        bids: vec![level(i64::MAX, i64::MAX, 1), level(1, i64::MAX, 1)],
        ask_orders: None,
        bid_orders: None,
    };
    let e = Env { book: big, own: vec![], ..env(&m) };
    for side in [Side::Buy, Side::Sell] {
        let mk = MarketIntent { side, amount: TOK, tip: 0, min_fill: None, slippage_bps: Some(9_999), all_or_nothing: false };
        let _ = plan_order(&e, &Intent::Market(mk.clone()));
        let _ = plan_order(&e, &Intent::Limit(limit(side, 10 * TOK, 250_000_000)));
        let _ = plan_order(&e, &Intent::Market(MarketIntent { all_or_nothing: true, ..mk }));
    }
    let row = |id: &str| serde_json::json!({"covenant_id": id, "price": i64::MAX.to_string(), "amount_left": i64::MAX.to_string()});
    let v = serde_json::json!({"asks": [row("a"), row("b")], "bids": [row("c"), row("d")]});
    let b = super::data::book_from_indexer(1000, &v);
    assert_eq!(b.asks.len(), 1);
    let agg = |p: &str| serde_json::json!({"price": p, "amount": i64::MAX.to_string(), "orders": i64::MAX});
    let v = serde_json::json!({"asks": [agg("5"), agg("5")], "bids": []});
    let _ = super::data::book_from_indexer(1000, &v);
    let pl = |n: &str| serde_json::json!({"price_num": n, "price_den": "1", "amount": i64::MAX.to_string(), "orders": i64::MAX});
    let huge = "170141183460469231731687303715884105727"; // i128::MAX
    let v = serde_json::json!({"asks": [pl(huge), pl("7"), pl("7")], "bids": [pl("7"), pl("7")]});
    let b = super::data::pair_book_from(&v, 1_000_000_000);
    assert!(b.asks.iter().all(|l| l.price > 0));
    let wide = serde_json::json!({"price_num": huge, "price_den": huge, "amount": "1"});
    let b = super::data::pair_book_from(&serde_json::json!({"asks": [wide], "bids": []}), 1);
    assert!(b.asks.iter().all(|l| l.price > 0));
    let _ = super::data::firm_reference(&b, 1);
}

/// `kob replace` of a stop-only sell (no take-profit leg) with a tip: the tip is bounded by the stop band's floor, not by a
/// missing take-profit (it refused every amendment of such an order).
#[test]
fn a_stop_only_sell_with_a_tip_can_be_amended() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    let p = valid(
        &e,
        Intent::Cond(CondIntent { stop: Some(230_000_000), tip: 1_000_000, ..cond(CondKind::StopMarket, Side::Sell, 10 * TOK) }),
    );
    let live = super::data::Live {
        cov: [0xa1; 32],
        order: kob_protocol::tx::OrderUtxo {
            utxo: Utxo { transaction_id: txid(9), index: 0, amount: C as u64, block_daa_score: 500, covenant_id: Some([0xa1; 32]) },
            state: p.states[0].clone(),
        },
        custody: None,
        prefund: None,
        strays: vec![],
        foreign: 0,
        deadline: None,
        genesis_daa: None,
    };
    let input = super::actions::AmendInput { stop: Some(220_000_000), ..Default::default() };
    let (rep, _, _) = super::actions::replacement(&live, &input, CLOCK.daa, 1).unwrap();
    let AnyState::KobCondAsk(c) = rep else { panic!("{rep:?}") };
    assert_eq!((c.stop_price, c.tip), (220_000_000, 1_000_000));
    // a tip at the stop band's floor is still refused
    let input = super::actions::AmendInput { tip: Some(230_000_000), ..Default::default() };
    assert!(super::actions::replacement(&live, &input, CLOCK.daa, 1).unwrap_err().contains("below every leg price"));
}
