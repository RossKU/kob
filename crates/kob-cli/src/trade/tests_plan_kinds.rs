//! The order kinds `kob place` gained for parity with the web ticket, against the numbers the web planner tests assert for
//! the same intents and fixtures (`web/src/kob/orders/simple.test.ts` streaming / close / TWAP / DCA / Dutch,
//! `cond.test.ts` trailing stop, `cond-ifd.test.ts` stop entries and repeat IFD / IFO, `pair-cond.test.ts`,
//! `pair-ifd.test.ts`), and the golden vectors (`create.ask.twap`, `create.bid.dca`, `create.ask.dutch`). Every plan is
//! signed, finalized, validated in the script engine and its placement record recovered (`tests_plan::valid`).

use kob_protocol::artifacts::TemplateId;
use kob_protocol::defaults::pair_tips;
use kob_protocol::state::{AnyState, AskState, BidState, CondAskState, CondBidState, CondPairState, IfdPairState};

use super::market::TokenMarket;
use super::plan::*;
use super::tests_plan::*;

const P: i64 = 250_000_000;
const KT: i64 = 2_200_000;

fn ask_of(s: &AnyState) -> AskState {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.clone(),
        o => panic!("not an ask: {o:?}"),
    }
}
fn bid_of(s: &AnyState) -> BidState {
    match s {
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.clone(),
        o => panic!("not a bid: {o:?}"),
    }
}
fn cask(s: &AnyState) -> CondAskState {
    match s {
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.clone(),
        o => panic!("not a KobCondAsk: {o:?}"),
    }
}
fn cbid(s: &AnyState) -> CondBidState {
    match s {
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.clone(),
        o => panic!("not a KobCondBid: {o:?}"),
    }
}
fn errors(p: &Plan) -> Vec<&'static str> {
    p.issues.iter().filter(|i| i.severity == Severity::Error).map(|i| i.code).collect()
}
fn value(p: &Plan) -> i64 {
    p.request.as_ref().unwrap().value as i64
}

fn streaming(side: Side, displayed: i64, tol: i64, aon: bool) -> Intent {
    Intent::Streaming(StreamingIntent {
        side,
        amount: 5 * TOK,
        tip: 0,
        min_fill: None,
        displayed_price: displayed,
        tolerance_bps: tol,
        all_or_nothing: aon,
    })
}

#[test]
fn streaming_starts_from_the_displayed_price() {
    let m = market(TemplateId::Kcc20Ref8x8);
    let e = Env {
        book: Book { asks: vec![level(250_000_000, 30 * TOK, 2)], bids: vec![level(245_000_000, 30 * TOK, 2)], ..Default::default() },
        ..env(&m)
    };
    let ioc = valid(&e, streaming(Side::Buy, 251_000_000, 50, false));
    let s = bid_of(&ioc.states[0]);
    assert_eq!((s.price, s.price_end, s.tif, s.min_fill), (251_000_000, 252_255_000, 1, 1));
    let d = ioc.disclosure.as_ref().unwrap();
    assert_eq!((d.expected_price, d.worst_price), (Some(251_000_000), Some(252_255_000)));
    assert!(d.notes.contains(&"streaming"));
    let fok = valid(&e, streaming(Side::Sell, 244_000_000, 100, true));
    let s = ask_of(&fok.states[0]);
    assert_eq!((s.price, s.price_end, s.tif), (244_000_000, 241_560_000, 2));
    assert!(codes(&plan_order(&e, &streaming(Side::Buy, 0, 50, false))).contains(&"REFERENCE_PRICE_INVALID"));
    assert!(codes(&plan_order(&e, &streaming(Side::Buy, 251_000_000, 0, false))).contains(&"SLIPPAGE_INVALID"));
}

fn close(amount: Option<i64>) -> Intent {
    Intent::Close(CloseIntent { amount, tip: 0, slippage_bps: None, all_or_nothing: false })
}

#[test]
fn close_sells_the_whole_balance_as_a_market_auction() {
    let m = market(TemplateId::Kcc20Ref8x8);
    let e = Env { token_utxos: vec![token_utxo(&m, 2, 7 * TOK), token_utxo(&m, 3, 3 * TOK + 250)], ..env(&m) };
    let p = valid(&e, close(None));
    let s = ask_of(&p.states[0]);
    assert_eq!((s.amount_left, s.tif, s.min_fill), (10_250, 1, 1));
    let d = p.disclosure.as_ref().unwrap();
    assert_eq!(d.amount, 10_250);
    assert!(d.notes.contains(&"close"));
    assert_eq!(p.built.as_ref().unwrap().tx.inputs.iter().filter(|i| i.utxo.covenant_id == Some(m.cov)).count(), 2);
    // a partial close, dust, nothing held, more than held
    let m = market(TemplateId::Kcc20Ref);
    assert_eq!(ask_of(&valid(&env(&m), close(Some(4 * TOK))).states[0]).amount_left, 4000);
    let dust = Env { token_utxos: vec![token_utxo(&m, 2, 999)], ..env(&m) };
    assert_eq!(ask_of(&valid(&dust, close(None)).states[0]).amount_left, 999);
    let none = Env { token_utxos: vec![], ..env(&m) };
    assert_eq!(codes(&plan_order(&none, &close(None))), vec!["CLOSE_NOTHING_TO_SELL"]);
    let few = Env { token_utxos: vec![token_utxo(&m, 2, 3 * TOK)], ..env(&m) };
    assert!(codes(&plan_order(&few, &close(Some(4 * TOK)))).contains(&"INSUFFICIENT_TOKENS"));
}

fn sched(dca: bool, amount: i64, slice: i64, interval: i64, price: i64) -> ScheduleIntent {
    ScheduleIntent {
        dca,
        amount,
        tip: 0,
        min_fill: None,
        slice,
        interval_daa: interval,
        price,
        price_end: None,
        slice_auction_daa: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        max_fills: None,
    }
}

#[test]
fn twap_and_dca_like_the_web_planner() {
    for m in markets() {
        let e = env(&m);
        // TWAP: the 10-KAS minimum at the limit (4 tokens) is above the 2-token slice: capped at the slice
        let p = valid(&e, Intent::Schedule(ScheduleIntent { tip: 100_000, ..sched(false, 10 * TOK, 2 * TOK, 600, P) }));
        let s = ask_of(&p.states[0]);
        assert_eq!(
            (s.interval, s.max_fill, s.min_fill, s.slope, s.price_end, s.tif, s.amount_left, s.tip, s.price),
            (600, 2000, 2000, 0, 0, 0, 10_000, 100_000, P)
        );
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!(d.tokens_escrowed, 10_000);
        assert!(d.notes.contains(&"twap"));
        // TWAP with a slice auction
        let p = valid(
            &e,
            Intent::Schedule(ScheduleIntent {
                price_end: Some(250_000_000),
                slice_auction_daa: Some(100),
                ..sched(false, 6 * TOK, 3 * TOK, 1_200, 260_000_000)
            }),
        );
        let s = ask_of(&p.states[0]);
        assert_eq!(
            (s.interval, s.max_fill, s.price, s.price_end, s.slope, s.decay_step),
            (1200, 3000, 260_000_000, 250_000_000, 100_000, 1)
        );
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.worst_price, d.expected_price), (Some(250_000_000), Some(260_000_000)));
        assert!(d.notes.contains(&"auction"));
        // DCA: one delivery carrier per slice (golden bid.dca: 6225.5 KAS + 4 sompi of rounding)
        let p = valid(&e, Intent::Schedule(ScheduleIntent { tip: 100_000, ..sched(true, 5 * TOK, TOK, 600, 245_000_000) }));
        let s = bid_of(&p.states[0]);
        assert_eq!((s.interval, s.max_fill, s.min_fill, s.slope, s.tif), (600, 1000, 1000, 0, 0));
        assert_eq!(value(&p), 6_225_500_004);
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!(
            d.carriers.iter().map(|c| (c.kind, c.amount, c.count)).collect::<Vec<_>>(),
            vec![("escrow", 5 * 245_100_000 + 4, 1), ("delivery carrier", 10 * KAS as i128, 5)]
        );
        // a rising slice auction budgets at the cap; extra fills add carriers
        let p = valid(
            &e,
            Intent::Schedule(ScheduleIntent {
                price_end: Some(250_000_000),
                max_fills: Some(6),
                ..sched(true, 4 * TOK, 2 * TOK, 600, 245_000_000)
            }),
        );
        let s = bid_of(&p.states[0]);
        assert_eq!((s.price, s.price_end, s.slope), (245_000_000, 250_000_000, 25_000));
        assert_eq!(value(&p), s.escrow(4 * TOK, 6).unwrap());
        let few = Intent::Schedule(ScheduleIntent { max_fills: Some(1), ..sched(true, 4 * TOK, 2 * TOK, 600, P) });
        assert!(codes(&plan_order(&e, &few)).contains(&"MAX_FILLS_INVALID"));
        // one slice; a schedule beyond the order's life; bad inputs
        let one = valid(&e, Intent::Schedule(sched(false, 3 * TOK, 10 * TOK, 600, P)));
        assert_eq!(ask_of(&one.states[0]).max_fill, 3000);
        assert!(codes(&one).contains(&"TWAP_SINGLE_SLICE"));
        let long = Intent::Schedule(ScheduleIntent {
            lifetime: Lifetime::GtdUnix(CLOCK.unix + 86_400),
            ..sched(false, 100 * TOK, TOK, 36_000, P)
        });
        assert!(codes(&plan_order(&e, &long)).contains(&"TWAP_EXCEEDS_LIFE"));
        assert!(codes(&plan_order(&e, &Intent::Schedule(sched(false, 5 * TOK, 0, 600, P)))).contains(&"SLICE_AMOUNT_INVALID"));
        assert!(codes(&plan_order(&e, &Intent::Schedule(sched(false, 5 * TOK, TOK, 0, P)))).contains(&"DURATION_INVALID"));
        let up = Intent::Schedule(ScheduleIntent { price_end: Some(P + 100), ..sched(false, 5 * TOK, TOK, 10, P) });
        assert!(codes(&plan_order(&e, &up)).contains(&"PRICE_END_INVALID"));
        let down = Intent::Schedule(ScheduleIntent { price_end: Some(P - 100), ..sched(true, 5 * TOK, TOK, 10, P) });
        assert!(codes(&plan_order(&e, &down)).contains(&"PRICE_END_INVALID"));
    }
}

#[test]
fn twap_and_dca_equal_the_golden_vectors() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    let p =
        valid(&e, Intent::Schedule(ScheduleIntent { tip: 100_000, min_fill: Some(1000), ..sched(false, 10 * TOK, 2 * TOK, 600, P) }));
    let (AnyState::KobAsk(g), _) = golden("create.ask.twap") else { panic!() };
    let s = ask_of(&p.states[0]);
    // equal but for what the vector fixes differently: maker, expiry, decay step
    assert_eq!(s, AskState { maker: s.maker, expiry_daa: s.expiry_daa, decay_step: s.decay_step, ..g });
    let p = valid(
        &e,
        Intent::Schedule(ScheduleIntent { tip: 100_000, min_fill: Some(1000), ..sched(true, 5 * TOK, TOK, 600, 245_000_000) }),
    );
    let (AnyState::KobBid(g), gv) = golden("create.bid.dca") else { panic!() };
    let s = bid_of(&p.states[0]);
    assert_eq!(s, BidState { maker: s.maker, expiry_daa: s.expiry_daa, decay_step: s.decay_step, ..g });
    assert_eq!(value(&p) as u64, gv);
}

fn dutch(side: Side, amount: i64, price: i64, end: i64, duration: i64) -> DutchIntent {
    DutchIntent {
        side,
        amount,
        tip: 0,
        min_fill: None,
        price,
        price_end: end,
        duration_daa: duration,
        step_daa: None,
        lifetime: Lifetime::Gtc,
        active_from: None,
        max_fills: None,
    }
}

#[test]
fn dutch_decay_and_rising_bid_like_the_web_planner() {
    for m in markets() {
        let e = env(&m);
        let p = valid(&e, Intent::Dutch(dutch(Side::Sell, 5 * TOK, 300_000_000, 200_000_000, 6_000)));
        let s = ask_of(&p.states[0]);
        assert_eq!(
            (s.price, s.price_end, s.decay_step, s.active_from, s.tif, s.min_fill),
            (300_000_000, 200_000_000, 1, CLOCK.daa + 30, 0, 5000)
        );
        assert!(s.slope * 6_000 >= 100_000_000 && s.slope * 5_999 < 100_000_000 + s.slope);
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.expected_price, d.worst_price, d.limit_price), (Some(300_000_000), Some(200_000_000), Some(200_000_000)));
        assert!(d.notes.contains(&"auction") && d.notes.contains(&"dutch"));
        // the golden shape: step 1000 DAA, slope 1_000_000 over 100_000 DAA
        let p = valid(
            &e,
            Intent::Dutch(DutchIntent {
                step_daa: Some(1_000),
                tip: 100_000,
                ..dutch(Side::Sell, 10 * TOK, 300_000_000, 200_000_000, 100_000)
            }),
        );
        let s = ask_of(&p.states[0]);
        assert_eq!((s.slope, s.decay_step, s.price, s.price_end), (1_000_000, 1_000, 300_000_000, 200_000_000));
        // a rising bid: escrow at the cap
        let p = valid(
            &e,
            Intent::Dutch(DutchIntent {
                step_daa: Some(10),
                max_fills: Some(2),
                ..dutch(Side::Buy, 5 * TOK, 200_000_000, 240_000_000, 4_000)
            }),
        );
        let s = bid_of(&p.states[0]);
        assert_eq!((s.price, s.price_end, s.slope, s.decay_step), (200_000_000, 240_000_000, 100_000, 10));
        assert_eq!(value(&p), s.escrow(5 * TOK, 2).unwrap());
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.expected_price, d.worst_price, d.all_in_price), (Some(200_000_000), Some(240_000_000), Some(240_000_000)));
        // a timed start; the end on the wrong side; bad duration / step; a day order
        let p = valid(
            &e,
            Intent::Dutch(DutchIntent {
                active_from: Some(Activation::Daa(CLOCK.daa + 999)),
                ..dutch(Side::Sell, 2 * TOK, 300_000_000, 290_000_000, 100)
            }),
        );
        assert_eq!(ask_of(&p.states[0]).active_from, CLOCK.daa + 999);
        assert_eq!(p.disclosure.as_ref().unwrap().activates_at.map(|a| a.0), Some(CLOCK.daa + 999));
        let c = |i: DutchIntent| codes(&plan_order(&e, &Intent::Dutch(i)));
        assert!(c(dutch(Side::Sell, 2 * TOK, 300_000_000, 300_000_000, 100)).contains(&"PRICE_END_INVALID"));
        assert!(c(dutch(Side::Buy, 2 * TOK, 300_000_000, 290_000_000, 100)).contains(&"PRICE_END_INVALID"));
        assert!(c(dutch(Side::Sell, 2 * TOK, 300_000_000, 290_000_000, 0)).contains(&"DURATION_INVALID"));
        assert!(c(DutchIntent { step_daa: Some(0), ..dutch(Side::Sell, 2 * TOK, 300_000_000, 290_000_000, 10) })
            .contains(&"DURATION_INVALID"));
        let day = valid(
            &e,
            Intent::Dutch(DutchIntent { lifetime: Lifetime::Day, ..dutch(Side::Sell, 2 * TOK, 300_000_000, 290_000_000, 100) }),
        );
        assert_eq!(day.request.as_ref().unwrap().deadline, Some(1_790_726_400));
    }
    // the golden dutch vector (its activation and minimum fill are fixed, the rest is the planner's)
    let m = market(TemplateId::Kcc20Ref);
    let p = valid(
        &env(&m),
        Intent::Dutch(DutchIntent {
            step_daa: Some(1_000),
            tip: 100_000,
            min_fill: Some(1000),
            ..dutch(Side::Sell, 10 * TOK, 300_000_000, 200_000_000, 100_000)
        }),
    );
    let (AnyState::KobAsk(g), _) = golden("create.ask.dutch") else { panic!() };
    let s = ask_of(&p.states[0]);
    assert_eq!(s, AskState { maker: s.maker, expiry_daa: s.expiry_daa, active_from: s.active_from, ..g });
}

fn trailing(side: Side, amount: i64, stop: i64, trail: Trail) -> CondIntent {
    CondIntent { stop: Some(stop), trail: Some(trail), ..cond(CondKind::Trailing, side, amount) }
}
const TR: Trail = Trail { step: 1_000_000, gap: 5_000_000, wait: None, updates: None };

#[test]
fn trailing_stops_like_the_web_planner() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    // sell: step, gap and wait in the state; 20 updates and the arm inside the 10 KAS carrier
    let p = valid(&e, Intent::Cond(trailing(Side::Sell, 10 * TOK, 230_000_000, TR)));
    let s = cask(&p.states[0]);
    assert_eq!(
        (s.trail_step, s.trail_gap, s.trail_wait, s.stop_price, s.tp_price, s.armed),
        (1_000_000, 5_000_000, 6_000, 230_000_000, 0, 0)
    );
    let d = p.disclosure.as_ref().unwrap();
    assert_eq!(d.kas_locked, 2 * C as i128);
    assert!(d.notes.contains(&"trailing") && d.notes.contains(&"keeperReserve"));
    // buy: the keeper reserve of 10 updates and the arm is added to the escrow
    let p = valid(&e, Intent::Cond(trailing(Side::Buy, 5 * TOK, 260_000_000, Trail { wait: Some(3_000), updates: Some(10), ..TR })));
    let s = cbid(&p.states[0]);
    assert_eq!((s.trail_step, s.trail_gap, s.trail_wait), (1_000_000, 5_000_000, 3_000));
    assert_eq!(value(&p), 5 * 267_800_000 + 2 * C + 11 * KT);
    assert!(p
        .disclosure
        .as_ref()
        .unwrap()
        .carriers
        .iter()
        .any(|c| c.kind == "keeper reserve" && c.amount == KT as i128 && c.count == 11));
    // a take-profit caps the trail (a trailing OCO)
    let p = valid(&e, Intent::Cond(CondIntent { take_profit: Some(300_000_000), ..trailing(Side::Sell, 2 * TOK, 230_000_000, TR) }));
    let s = cask(&p.states[0]);
    assert_eq!((s.tp_price, s.stop_price, s.trail_step), (300_000_000, 230_000_000, 1_000_000));
    let d = p.disclosure.as_ref().unwrap();
    assert_eq!((d.limit_price, d.worst_price), (Some(300_000_000), Some(223_100_000)));
    // refusals: below the minimum interval, off the tick, a reserve beyond the carrier (fine on a larger one)
    let bad = |t: Trail, e: &Env| errors(&plan_order(e, &Intent::Cond(trailing(Side::Sell, TOK, 230_000_000, t))));
    assert_eq!(bad(Trail { wait: Some(100), ..TR }, &e), vec!["COND_TRAIL_WAIT_TOO_SHORT"]);
    assert_eq!(bad(Trail { step: 1_000_050, gap: 0, wait: None, updates: None }, &e), vec!["PRICE_NOT_ON_TICK"]);
    assert_eq!(bad(Trail { updates: Some(1_000), ..TR }, &e), vec!["COND_KEEPER_FUNDING_TOO_LARGE"]);
    let big = Env { carrier: Some(30 * KAS as u64), ..env(&m) };
    assert!(bad(Trail { updates: Some(1_000), ..TR }, &big).is_empty());
    assert_eq!(bad(Trail { step: 0, ..TR }, &e), vec!["COND_TRAIL_STEP_INVALID"]);
}

fn rep(side: Side, price: i64, tp: i64, count: Option<i64>) -> IfdIntent {
    IfdIntent { more: IfdMore { repeat: Some(count), ..Default::default() }, ..ifd(side, price, Some(tp), None) }
}

#[test]
fn stop_entries_and_repeats_like_the_web_planner() {
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    // a buy-stop entry: armed by a touch, an auction from the stop to the limit, the arming tip on top
    let se = |price, stop| IfdIntent {
        more: IfdMore { entry_stop: Some(stop), ..Default::default() },
        ..ifd(Side::Buy, price, Some(300_000_000), None)
    };
    let p = valid(&e, Intent::Ifd(se(260_000_000, 255_000_000)));
    let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &p.states[0] else { panic!() };
    assert_eq!((s.entry_stop, s.band_daa, s.keeper_tip, s.min_touch, s.min_rest_daa, s.armed), (255_000_000, 300, KT, 2500, 50, 0));
    assert_eq!(value(&p), 10 * 260_000_000 + 8 * C + KT);
    let d = p.disclosure.as_ref().unwrap();
    assert_eq!(
        (d.limit_price, d.expected_price, d.worst_price, d.keeper_tip, d.min_touch),
        (Some(260_000_000), Some(255_000_000), Some(260_000_000), KT, Some(2500))
    );
    assert!(d.carriers.iter().any(|c| c.kind == "keeper reserve" && c.amount == KT as i128 && c.count == 1));
    assert!(["stopEntry", "stopEntryAuction", "triggerExposure"].iter().all(|n| d.notes.contains(n)));
    let flat = valid(&e, Intent::Ifd(se(260_000_000, 260_000_000)));
    let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &flat.states[0] else { panic!() };
    assert_eq!((s.entry_stop, s.band_daa), (260_000_000, 0));
    // the knobs
    let knobs = IfdIntent {
        more: IfdMore {
            entry_stop: Some(255_000_000),
            entry_knobs: StopKnobs { band_daa: Some(100), keeper_tip: Some(1_000_000), min_rest: Some(900) },
            entry_min_touch: Some(3 * TOK),
            ..Default::default()
        },
        ..ifd(Side::Buy, 260_000_000, Some(300_000_000), None)
    };
    let p = valid(&e, Intent::Ifd(knobs));
    let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &p.states[0] else { panic!() };
    assert_eq!((s.band_daa, s.keeper_tip, s.min_touch, s.min_rest_daa), (100, 1_000_000, 3000, 900));
    // a sell-stop entry: the arming keeper is paid from the entry carrier
    let p = valid(
        &e,
        Intent::Ifd(IfdIntent {
            more: IfdMore { entry_stop: Some(244_000_000), ..Default::default() },
            ..ifd(Side::Sell, 240_000_000, Some(200_000_000), None)
        }),
    );
    let (AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s)) = &p.states[0] else { panic!() };
    assert_eq!((s.entry_stop, s.band_daa, s.keeper_tip, s.min_touch, s.prefund), (244_000_000, 300, KT, 2500, 0));
    assert_eq!(value(&p), C + 4 * C + 3);
    // a stop beyond the limit
    assert_eq!(errors(&plan_order(&e, &Intent::Ifd(se(260_000_000, 260_000_100)))), vec!["COND_ENTRY_STOP_BEYOND_LIMIT"]);
    // a trailing stop-limit exit, sell first: the band within the limit, the prefund covers the worst buy-back
    let p = valid(
        &e,
        Intent::Ifd(IfdIntent {
            amount: 4 * TOK,
            stop_limit: Some(285_000_000),
            more: IfdMore {
                exit_trail: Some(Trail { step: 1_000_000, gap: 3_000_000, wait: None, updates: None }),
                ..Default::default()
            },
            ..ifd(Side::Sell, 260_000_000, Some(240_000_000), Some(280_000_000))
        }),
    );
    let x = cbid(&p.states[1]);
    assert_eq!((x.trail_step, x.trail_gap, x.slip_bps), (1_000_000, 3_000_000, 178));
    let (AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s)) = &p.states[0] else { panic!() };
    assert_eq!(s.prefund, 280_000_000 + 28_000 * 178 - 260_000_000);

    // repeat buy-first: rptAmount = 1 + K * N, the merge cost in the exit tip, one more exit carrier
    let mt_buy = merge_tip_rate(MERGE_COST_BUY_FIRST, 1000, 2_500);
    let mt_sell = merge_tip_rate(MERGE_COST_SELL_FIRST, 1000, 2_500);
    assert_eq!((mt_buy, mt_sell), (400_000, 600_000));
    let p = valid(&e, Intent::Ifd(rep(Side::Buy, 240_000_000, 300_000_000, Some(2))));
    let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &p.states[0] else { panic!() };
    assert_eq!(s.rpt_amount, 20_001);
    let x = cask(&p.states[1]);
    assert_eq!((x.tip, x.parent, x.rpt_price, x.amount_left), (mt_buy, [0; 32], 0, 10_000));
    assert_eq!(s.exit().unwrap(), x);
    assert_eq!(value(&p), 10 * 240_000_000 + 8 * C + C);
    let d = p.disclosure.as_ref().unwrap();
    assert_eq!(
        d.carriers.iter().map(|c| (c.kind, c.amount, c.count)).collect::<Vec<_>>(),
        vec![("escrow", 2_400_000_000, 1), ("delivery carrier", C as i128, 4), ("exit carrier", C as i128, 5)]
    );
    assert!(["repeat", "repeatCounted", "repeatReBuys", "mergeTip", "cancelPosition"].iter().all(|n| d.notes.contains(n)));
    // sell-first: the larger merge cost; the entry value has no extra carrier
    let p = valid(&e, Intent::Ifd(rep(Side::Sell, 260_000_000, 240_000_000, Some(2))));
    let (AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s)) = &p.states[0] else { panic!() };
    assert_eq!(s.rpt_amount, 20_001);
    assert_eq!(cbid(&p.states[1]).tip, mt_sell);
    assert_eq!(s.exit().unwrap(), cbid(&p.states[1]));
    assert_eq!(value(&p), C + 4 * C + 3);
    // the exit tip adds to the merge cost; unlimited; unprofitable take-profits; a count below one
    let p = valid(&e, Intent::Ifd(IfdIntent { exit_tip: 25_000, tip: 5_000, ..rep(Side::Buy, 240_000_000, 300_000_000, Some(2)) }));
    assert_eq!(cask(&p.states[1]).tip, mt_buy + 25_000);
    let p = valid(&e, Intent::Ifd(rep(Side::Buy, 240_000_000, 300_000_000, None)));
    let (AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s)) = &p.states[0] else { panic!() };
    assert_eq!(s.rpt_amount, 1 + 10_000_000 * 10_000);
    assert!(p.disclosure.as_ref().unwrap().notes.contains(&"repeatUnlimited"));
    assert_eq!(
        errors(&plan_order(&e, &Intent::Ifd(rep(Side::Buy, 240_000_000, 240_400_000, Some(2))))),
        vec!["COND_TP_NOT_PROFITABLE"]
    );
    assert!(plan_order(&e, &Intent::Ifd(rep(Side::Buy, 240_000_000, 240_400_100, Some(2)))).ok);
    assert_eq!(
        errors(&plan_order(&e, &Intent::Ifd(rep(Side::Sell, 260_000_000, 259_400_000, Some(2))))),
        vec!["COND_TP_NOT_PROFITABLE"]
    );
    assert!(plan_order(&e, &Intent::Ifd(rep(Side::Sell, 260_000_000, 259_399_900, Some(2)))).ok);
    let plain = valid(&e, Intent::Ifd(ifd(Side::Buy, 240_000_000, Some(240_000_000), None)));
    assert_eq!(codes(&plain), vec!["COND_TP_NOT_PROFITABLE"]);
    for k in [0, -3] {
        assert_eq!(
            errors(&plan_order(&e, &Intent::Ifd(rep(Side::Buy, 240_000_000, 300_000_000, Some(k))))),
            vec!["COND_REPEAT_COUNT_INVALID"]
        );
    }
    // a repeating stop entry pre-funds one arm per cycle (K + 1), at most ten
    let p = valid(
        &e,
        Intent::Ifd(IfdIntent {
            more: IfdMore { entry_stop: Some(255_000_000), repeat: Some(Some(2)), ..Default::default() },
            ..ifd(Side::Buy, 260_000_000, Some(300_000_000), None)
        }),
    );
    assert!(p.disclosure.as_ref().unwrap().carriers.iter().any(|c| c.kind == "keeper reserve" && c.count == 3));
    // repeat IFO: a stop-loss next to the take-profit; the prefund covers the stop's worst buy-back with the merge tip
    let p = valid(
        &e,
        Intent::Ifd(IfdIntent {
            amount: 5 * TOK,
            more: IfdMore { repeat: Some(Some(3)), ..Default::default() },
            ..ifd(Side::Sell, 260_000_000, Some(240_000_000), Some(280_000_000))
        }),
    );
    let mts = merge_tip_rate(MERGE_COST_SELL_FIRST, 1000, 1_250);
    let (AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s)) = &p.states[0] else { panic!() };
    assert_eq!((s.rpt_amount, s.prefund), (15_001, 288_400_000 + mts - 260_000_000));
}

// ------------------------------------------------------------------------------------------------ pairs

fn tok_b() -> TokenMarket {
    TokenMarket::new([0x71; 32], "BBB", 2, TemplateId::Kcc20Ref8x8, Some([0xee; 32]), None).unwrap()
}
fn kron_a() -> TokenMarket {
    TokenMarket::new([0x72; 32], "KRA", 3, TemplateId::KronToken2433, None, None).unwrap()
}
fn kron_b() -> TokenMarket {
    TokenMarket::new([0x73; 32], "KRB", 0, TemplateId::KronToken2433, None, None).unwrap()
}
fn pair_combos() -> Vec<(TokenMarket, TokenMarket)> {
    let a = TokenMarket { tick: 1, ..market(TemplateId::Kcc20Ref) };
    vec![(a.clone(), tok_b()), (kron_a(), tok_b()), (a, kron_b()), (kron_a(), kron_b())]
}
/// The pair book of `pair-fixtures.ts`: best ask 1500, best bid 1450 (B per whole A).
fn pair_env(a: &TokenMarket, b: &TokenMarket) -> Env {
    Env {
        book: Book {
            asks: vec![level(1500, 5_000, 2), level(1510, 10_000, 3), level(1600, 20_000, 1)],
            bids: vec![level(1450, 5_000, 1), level(1440, 10_000, 2), level(1400, 20_000, 3)],
            ..Default::default()
        },
        funding: vec![key_utxo(1, 100_000 * KAS)],
        token_utxos: vec![token_utxo(a, 2, 100 * a.scale)],
        pair: Some(PairPart {
            quote: b.clone(),
            quote_utxos: vec![token_utxo(b, 3, 1_000_000 * b.scale)],
            kas_per_whole_a: Some(3 * KAS),
            kas_per_whole_b: Some(KAS / 5),
        }),
        ..env(a)
    }
}
fn ceil_q(n: i64, p: i64, s: i64) -> i64 {
    ((n as i128 * p as i128 + s as i128 - 1) / s as i128) as i64
}
fn floor_q(n: i64, p: i64, s: i64) -> i64 {
    (n as i128 * p as i128 / s as i128) as i64
}
fn def_min(amount: i64, scale: i64) -> i64 {
    ceil_q(10 * KAS, scale, 3 * KAS).min(amount)
}
fn cpair(p: &Plan) -> CondPairState {
    let AnyState::KobCondPair(s) = &p.states[0] else { panic!("{:?}", p.states[0]) };
    s.clone()
}
fn ipair(p: &Plan) -> IfdPairState {
    let AnyState::KobIfdPair(s) = &p.states[0] else { panic!("{:?}", p.states[0]) };
    s.clone()
}

#[test]
fn pair_conditionals_like_the_web_planner() {
    for (a, b) in pair_combos() {
        let e = pair_env(&a, &b);
        let kt = pair_tips(a.program, b.program).unwrap().keeper_tip as i64;
        let amount = 9_001;
        let tip = 77_777;
        // stop-market sell: side ASK holds the A amount, a 3 % band over 300 DAA, minTouch = minFill, the keeper on the order UTXO
        let p = valid(&e, Intent::Cond(CondIntent { stop: Some(1_401), tip, ..cond(CondKind::StopMarket, Side::Sell, amount) }));
        let s = cpair(&p);
        let mf = def_min(amount, a.scale);
        assert_eq!(
            (s.side, s.s_cov_id, s.t_cov_id, s.stop_price, s.tp_price, s.slip_bps, s.band_daa, s.min_rest_daa, s.armed),
            (1, a.cov, b.cov, 1_401, 0, 300, 300, 50, 0)
        );
        assert_eq!((s.custody, s.amount_left, s.keeper_tip, s.tip, s.min_fill, s.min_touch), (amount, amount, kt, tip, mf, mf));
        let worst = 1_401 - 1_401 * 300 / 10_000;
        let fills = ((amount + mf - 1) / mf).min(64);
        assert_eq!(value(&p), fills * C + floor_q(amount, tip, a.scale) + kt);
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!(
            (d.limit_price, d.worst_price, d.expected_price, d.min_touch, d.keeper_tip),
            (Some(worst), Some(worst), Some(1_401), Some(mf), kt)
        );
        assert_eq!(d.all_in_total, Some(ceil_q(amount, worst, a.scale)));
        assert!(["stopTrigger", "stopAuction", "triggerExposure", "keeperReserve"].iter().all(|n| d.notes.contains(n)));
        // stop-market buy: the B escrow at the band ceiling (rounded down) plus one unit
        let p = valid(&e, Intent::Cond(CondIntent { stop: Some(1_601), ..cond(CondKind::StopMarket, Side::Buy, amount) }));
        let s = cpair(&p);
        let worst = 1_601 + 1_601 * 300 / 10_000;
        assert_eq!((s.side, s.s_cov_id, s.t_cov_id), (2, b.cov, a.cov));
        assert_eq!(s.custody, floor_q(amount, worst, a.scale) + 1);
        assert_eq!(p.disclosure.as_ref().unwrap().quote_escrowed, s.custody);
        // stop-limit, take-profit, OCO
        let p = valid(
            &e,
            Intent::Cond(CondIntent { stop: Some(1_400), limit: Some(1_380), ..cond(CondKind::StopLimit, Side::Sell, amount) }),
        );
        assert_eq!(cpair(&p).slip_bps, (21 * 10_000 - 1) / 1_400);
        assert_eq!(p.disclosure.as_ref().unwrap().all_in_total, Some(ceil_q(amount, 1_380, a.scale)));
        let p = valid(&e, Intent::Cond(CondIntent { take_profit: Some(1_300), ..cond(CondKind::TakeProfit, Side::Buy, amount) }));
        let s = cpair(&p);
        assert_eq!((s.tp_price, s.stop_price, s.keeper_tip, s.slip_bps, s.band_daa), (1_300, 0, 0, 0, 0));
        assert_eq!(s.custody, floor_q(amount, 1_300, a.scale) + 1);
        let p = valid(
            &e,
            Intent::Cond(CondIntent { stop: Some(1_400), take_profit: Some(1_700), ..cond(CondKind::Oco, Side::Sell, amount) }),
        );
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.limit_price, d.worst_price, d.expected_price), (Some(1_700), Some(1_400 - 42), None));
        // a trailing stop with a take-profit: 20 updates and the arm pre-funded on the order UTXO
        let p = valid(
            &e,
            Intent::Cond(CondIntent {
                take_profit: Some(1_800),
                ..trailing(Side::Sell, amount, 1_400, Trail { step: 10, gap: 30, wait: None, updates: None })
            }),
        );
        let s = cpair(&p);
        assert_eq!((s.trail_step, s.trail_gap, s.trail_wait, s.tp_price), (10, 30, 6_000, 1_800));
        assert!(p
            .disclosure
            .as_ref()
            .unwrap()
            .carriers
            .iter()
            .any(|c| c.kind == "keeper reserve" && c.amount == kt as i128 && c.count == 21));
    }
    // presets, warnings and refusals
    let (a, b) = (TokenMarket { tick: 1, ..market(TemplateId::Kcc20Ref) }, tok_b());
    let e = pair_env(&a, &b);
    let p = valid(
        &e,
        Intent::Cond(CondIntent {
            stop: Some(1_400),
            min_touch: Some(4_000),
            slip_bps: Some(150),
            knobs: StopKnobs { band_daa: Some(0), keeper_tip: Some(5_000_000), min_rest: Some(100) },
            max_fills: Some(2),
            ..cond(CondKind::StopMarket, Side::Sell, 8_000)
        }),
    );
    let s = cpair(&p);
    assert_eq!((s.min_touch, s.min_rest_daa, s.band_daa, s.slip_bps, s.keeper_tip), (4_000, 100, 0, 150, 5_000_000));
    assert_eq!(value(&p), 2 * C + 5_000_000);
    let w = valid(&e, Intent::Cond(CondIntent { stop: Some(1_500), ..cond(CondKind::StopMarket, Side::Sell, 5_000) }));
    assert!(codes(&w).contains(&"COND_STOP_ALREADY_REACHED"));
    let w = valid(&e, Intent::Cond(CondIntent { take_profit: Some(1_510), ..cond(CondKind::TakeProfit, Side::Buy, 5_000) }));
    assert!(codes(&w).contains(&"PAIR_TP_CROSSES"));
    let bad = |i: CondIntent| errors(&plan_order(&e, &Intent::Cond(i)));
    assert_eq!(
        bad(CondIntent { stop: Some(1_400), take_profit: Some(1_300), ..cond(CondKind::Oco, Side::Sell, 5_000) }),
        vec!["COND_TP_STOP_ORDER"]
    );
    assert_eq!(
        bad(CondIntent { stop: Some(1_400), limit: Some(1_450), ..cond(CondKind::StopLimit, Side::Sell, 5_000) }),
        vec!["COND_STOP_LIMIT_BEYOND_STOP"]
    );
    assert_eq!(
        bad(trailing(Side::Sell, 5_000, 1_400, Trail { step: 0, gap: 0, wait: None, updates: None })),
        vec!["COND_TRAIL_STEP_INVALID"]
    );
    assert_eq!(
        bad(CondIntent { stop: Some(1_600), max_fills: Some(99), ..cond(CondKind::StopMarket, Side::Buy, 20_000) }),
        vec!["MAX_FILLS_INVALID"]
    );
    assert_eq!(bad(CondIntent { stop: Some(0), ..cond(CondKind::StopMarket, Side::Sell, 5_000) }), vec!["PRICE_NOT_POSITIVE"]);
    let kron = pair_env(&kron_a(), &kron_b());
    let p = plan_order(&kron, &Intent::Cond(CondIntent { stop: Some(2_000_000), ..cond(CondKind::StopMarket, Side::Buy, 1_000_000) }));
    assert_eq!(errors(&p), vec!["PAIR_KRON_CUSTODY_TOO_LARGE"]);
    let own = Env {
        own: vec![OwnOrder { cov: [0xcd; 32], side: Side::Buy, price: 1_390, tip: 0, amount_left: 100, active: true }],
        ..pair_env(&a, &b)
    };
    let sm = |slip| {
        plan_order(
            &own,
            &Intent::Cond(CondIntent { stop: Some(1_400), slip_bps: slip, ..cond(CondKind::StopMarket, Side::Sell, 5_000) }),
        )
    };
    assert_eq!(errors(&sm(None)), vec!["SELF_TRADE"]);
    assert!(!codes(&sm(Some(0))).contains(&"SELF_TRADE"));
}

#[test]
fn pair_if_done_like_the_web_planner() {
    for (a, b) in pair_combos() {
        let e = pair_env(&a, &b);
        let kt = pair_tips(a.program, b.program).unwrap().keeper_tip as i64;
        let amount = 8_003;
        let min_fill = (amount + 3) / 4;
        let fills = (amount + min_fill - 1) / min_fill;
        let exit_min = def_min(amount, a.scale).min(min_fill);
        let exit_fills = 2.min((amount + exit_min - 1) / exit_min);
        // IFD buy-first: the B escrow is the spend of the whole amount at the limit; every fill creates an ASK exit
        let tip = 50_001;
        let p = valid(&e, Intent::Ifd(IfdIntent { amount, tip, ..ifd(Side::Buy, 1_401, Some(1_601), None) }));
        let s = ipair(&p);
        assert_eq!(
            (s.side, s.a_cov_id, s.b_cov_id, s.a_scale, s.b_scale, s.price, s.prefund, s.tip),
            (2, a.cov, b.cov, a.scale, b.scale, 1_401, 0, tip)
        );
        assert_eq!(
            (s.min_fill, s.entry_stop, s.band_daa, s.min_touch, s.keeper_tip, s.rpt_amount, s.amount_left),
            (min_fill, 0, 0, 0, 0, 0, amount)
        );
        assert_eq!(s.custody, floor_q(amount, 1_401, a.scale));
        assert_eq!(s.exit_state.len(), 432);
        let committed = s.exit().unwrap();
        assert_eq!((committed.side, committed.tp_price, committed.stop_price, committed.min_fill), (1, 1_601, 0, exit_min));
        let AnyState::KobCondPair(x) = &p.states[1] else { panic!() };
        assert_eq!((x.amount_left, x.custody, x.side), (amount, amount, 1));
        assert_eq!(s.exit_carrier, C * exit_fills);
        assert_eq!(value(&p), fills * (C + s.exit_carrier) + floor_q(amount, tip, a.scale));
        assert_eq!(value(&p), s.kas_value().unwrap());
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.all_in_total, d.quote_escrowed, d.tokens_escrowed), (Some(floor_q(amount, 1_401, a.scale)), s.custody, 0));
        // IFO sell-first: A custody and a B prefund covering the exit's worst buy-back
        let p = valid(&e, Intent::Ifd(IfdIntent { amount, ..ifd(Side::Sell, 1_599, Some(1_399), Some(1_700)) }));
        let s = ipair(&p);
        // the worst buy-back: the stop band ceiling (above the take-profit)
        let prefund = 1_700 + 1_700 * 300 / 10_000 - 1_599;
        assert_eq!((s.side, s.price, s.prefund), (1, 1_599, prefund));
        assert_eq!(s.custody, ceil_q(amount, prefund, a.scale) + (fills - 1));
        let committed = s.exit().unwrap();
        assert_eq!(
            (committed.side, committed.tp_price, committed.stop_price, committed.slip_bps, committed.keeper_tip),
            (2, 1_399, 1_700, 300, kt)
        );
        let AnyState::KobCondPair(x) = &p.states[1] else { panic!() };
        assert_eq!(x.custody, ceil_q(amount, 1_599, a.scale) + ceil_q(amount, prefund, a.scale));
        assert_eq!(s.exit_carrier, C * exit_fills + kt);
        let d = p.disclosure.as_ref().unwrap();
        assert_eq!((d.tokens_escrowed, d.quote_escrowed, d.all_in_total), (amount, s.custody, Some(ceil_q(amount, 1_599, a.scale))));
        // a stop entry (buy-first): the trigger, the band, one arm's keeper reserve on the entry UTXO
        let p = valid(
            &e,
            Intent::Ifd(IfdIntent {
                amount,
                more: IfdMore { entry_stop: Some(1_550), ..Default::default() },
                ..ifd(Side::Buy, 1_600, None, Some(1_450))
            }),
        );
        let s = ipair(&p);
        assert_eq!((s.entry_stop, s.band_daa, s.min_touch, s.min_rest_daa, s.keeper_tip), (1_550, 300, min_fill, 50, kt));
        assert_eq!(value(&p), s.kas_value().unwrap() + kt);
    }
    // repeat on a pair: rptAmount, the pair merge tip, one more exit carrier; a loss is refused
    let (a, b) = (TokenMarket { tick: 1, ..market(TemplateId::Kcc20Ref) }, tok_b());
    let e = pair_env(&a, &b);
    let p = valid(
        &e,
        Intent::Ifd(IfdIntent {
            amount: 10_000,
            more: IfdMore { repeat: Some(Some(2)), ..Default::default() },
            ..ifd(Side::Buy, 1_400, Some(1_600), None)
        }),
    );
    let s = ipair(&p);
    assert_eq!(s.rpt_amount, 20_001);
    let exit_min = def_min(10_000, a.scale).min(2_500);
    assert_eq!(s.exit().unwrap().tip, merge_tip_rate(super::plan_pair::PAIR_MERGE_COST_BUY_FIRST, 1000, exit_min));
    let d = p.disclosure.as_ref().unwrap();
    assert!(d.carriers.iter().any(|c| c.kind == "exit carrier" && c.count == 5));
    let loss = plan_order(
        &e,
        &Intent::Ifd(IfdIntent {
            amount: 10_000,
            more: IfdMore { repeat: Some(Some(2)), ..Default::default() },
            ..ifd(Side::Buy, 1_400, Some(1_400), None)
        }),
    );
    assert_eq!(errors(&loss), vec!["PAIR_TP_NOT_PROFITABLE"]);
}

#[test]
fn every_simple_kind_runs_on_a_pair() {
    let (a, b) = (TokenMarket { tick: 1, ..market(TemplateId::Kcc20Ref) }, tok_b());
    let e = pair_env(&a, &b);
    // IOC, market (from the pair book's touch), TWAP, DCA and a Dutch decay are KobPair orders
    let p = valid(
        &e,
        Intent::Immediate(ImmediateIntent {
            fok: false,
            side: Side::Buy,
            amount: 5_000,
            price: 1_520,
            tip: 0,
            min_fill: None,
            life_daa: None,
            active_from: None,
        }),
    );
    let AnyState::KobPair(s) = &p.states[0] else { panic!() };
    assert_eq!((s.tif, s.price, s.side), (1, 1_520, 2));
    let p = valid(
        &e,
        Intent::Market(MarketIntent {
            side: Side::Sell,
            amount: 5_000,
            tip: 0,
            min_fill: None,
            slippage_bps: None,
            all_or_nothing: false,
        }),
    );
    let AnyState::KobPair(s) = &p.states[0] else { panic!() };
    assert_eq!((s.price, s.price_end, s.tif), (1_450, 1_450 - 1_450 * 300 / 10_000, 1));
    let p = valid(&e, Intent::Schedule(sched(false, 10_000, 2_000, 600, 1_600)));
    let AnyState::KobPair(s) = &p.states[0] else { panic!() };
    assert_eq!((s.interval, s.max_fill), (600, 2_000));
    // one delivery carrier per slice
    assert!(p.disclosure.as_ref().unwrap().carriers.iter().any(|c| c.kind == "delivery carrier" && c.count == 5));
    let p = valid(&e, Intent::Schedule(sched(true, 10_000, 5_000, 600, 1_400)));
    let AnyState::KobPair(s) = &p.states[0] else { panic!() };
    assert_eq!((s.side, s.max_fill), (2, 5_000));
    let p = valid(&e, Intent::Dutch(dutch(Side::Sell, 5_000, 1_700, 1_550, 1_000)));
    let AnyState::KobPair(s) = &p.states[0] else { panic!() };
    assert_eq!((s.price, s.price_end, s.active_from), (1_700, 1_550, CLOCK.daa + 30));
    // FOK against the pair book: refused when the book cannot fill it
    let fok = plan_order(
        &e,
        &Intent::Immediate(ImmediateIntent {
            fok: true,
            side: Side::Buy,
            amount: 100_000,
            price: 1_500,
            tip: 0,
            min_fill: None,
            life_daa: None,
            active_from: None,
        }),
    );
    assert!(codes(&fok).contains(&"FOK_INSUFFICIENT_DEPTH"));
}

/// `kob replace` re-plans an order from its own state (`replace-intent.ts`): with no change, the intent read back from
/// a planned order plans the same order again (trailing stops, stop entries with trailing stop-limit exits, pair
/// conditionals, pair if-done entries, pair TWAP); a change moves only what it names.
#[test]
fn replace_intents_round_trip() {
    use super::replan::{intent_from_order, lifetime_of, Changes};
    let same = |e: &Env, i: Intent| {
        let p = valid(e, i);
        let lt = lifetime_of(p.states[0].expiry().unwrap().0, Some(CLOCK.daa), None, false);
        assert_eq!(lt, Lifetime::Gtc);
        let back = intent_from_order(&p.states[0], lt, CLOCK.daa, &Changes::default()).unwrap();
        let q = valid(e, back);
        assert_eq!(q.states, p.states);
        assert_eq!(value(&q), value(&p));
        p
    };
    let m = market(TemplateId::Kcc20Ref);
    let e = env(&m);
    let t = same(&e, Intent::Cond(CondIntent { take_profit: Some(300_000_000), ..trailing(Side::Sell, 2 * TOK, 230_000_000, TR) }));
    same(&e, Intent::Cond(trailing(Side::Buy, 5 * TOK, 260_000_000, Trail { wait: Some(3_000), ..TR })));
    same(
        &e,
        Intent::Ifd(IfdIntent {
            amount: 4 * TOK,
            stop_limit: Some(285_000_000),
            more: IfdMore {
                entry_stop: Some(262_000_000),
                exit_trail: Some(Trail { step: 1_000_000, gap: 3_000_000, wait: None, updates: None }),
                ..Default::default()
            },
            ..ifd(Side::Sell, 260_000_000, Some(240_000_000), Some(280_000_000))
        }),
    );
    same(&e, Intent::Ifd(ifd(Side::Buy, 240_000_000, Some(300_000_000), Some(200_000_000))));
    let (a, b) = (TokenMarket { tick: 1, ..market(TemplateId::Kcc20Ref) }, tok_b());
    let pe = pair_env(&a, &b);
    same(&pe, Intent::Cond(CondIntent { stop: Some(1_400), take_profit: Some(1_700), ..cond(CondKind::Oco, Side::Sell, 9_001) }));
    same(&pe, Intent::Cond(CondIntent { stop: Some(1_400), limit: Some(1_380), ..cond(CondKind::StopLimit, Side::Sell, 9_001) }));
    same(&pe, Intent::Ifd(IfdIntent { amount: 8_003, ..ifd(Side::Sell, 1_599, Some(1_399), Some(1_700)) }));
    same(&pe, Intent::Ifd(IfdIntent { amount: 8_003, ..ifd(Side::Buy, 1_401, Some(1_601), None) }));
    same(
        &pe,
        Intent::Schedule(ScheduleIntent {
            price_end: Some(1_550),
            slice_auction_daa: Some(100),
            ..sched(false, 10_000, 2_000, 600, 1_600)
        }),
    );
    same(&pe, Intent::Limit(LimitIntent { min_fill: Some(1_000), ..limit(Side::Buy, 9_000, 1_400) }));
    // a change: the trailing stop moves, its trail and take-profit stay
    let ch = Changes { stop: Some(231_000_000), ..Default::default() };
    let back = intent_from_order(&t.states[0], Lifetime::Gtc, CLOCK.daa, &ch).unwrap();
    let s = cask(&valid(&e, back).states[0]);
    assert_eq!((s.stop_price, s.tp_price, s.trail_step, s.trail_gap), (231_000_000, 300_000_000, 1_000_000, 5_000_000));
    // a Dutch decay replaced later goes on from where it is now
    let d = valid(&pe, Intent::Dutch(dutch(Side::Sell, 5_000, 1_700, 1_500, 2_000)));
    let AnyState::KobPair(x) = &d.states[0] else { panic!() };
    let later = x.active_from + 100;
    let Intent::Dutch(di) = intent_from_order(&d.states[0], Lifetime::Gtc, later, &Changes::default()).unwrap() else { panic!() };
    assert_eq!((di.price, di.price_end, x.slope), (1_600, 1_500, 1));
    let ended = intent_from_order(&d.states[0], Lifetime::Gtc, x.active_from + 5_000, &Changes::default()).unwrap();
    assert!(matches!(ended, Intent::Limit(LimitIntent { price: 1_500, .. })));
    // refused: a repeating entry
    let r = valid(&e, Intent::Ifd(rep(Side::Buy, 240_000_000, 300_000_000, Some(2))));
    assert!(intent_from_order(&r.states[0], Lifetime::Gtc, CLOCK.daa, &Changes::default()).unwrap_err().contains("repeating"));
}
