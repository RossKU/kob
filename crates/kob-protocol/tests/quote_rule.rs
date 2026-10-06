//! The protocol v3 quote rule through the builders AND the rusty-kaspa v2.1.0 engine: amounts in base units, prices per
//! whole token (`scale` base units), every quote rounded in the maker's favour, the minimum fill of every kind, the
//! covenant arithmetic's overflow limits and the repeat merge argument.
//!
//! Where only the covenant can be shown to refuse (the builders refuse first), the test builds a valid transaction for a
//! state the covenant accepts and patches the order input's state (its signing plan and its UTXO script, and every
//! output derived from it) to the state under test: the engine must then fail that input, and only that state changed.

mod common;

use common::exact::{build_measured, patched_outcomes, run_any};
use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{merge_arg, Action, Batch, Leg};
use kob_protocol::state::*;
use kob_protocol::tx::{spk_to_string, Arg, SigPlan, TokenUtxo};

const P: TemplateId = TemplateId::Kcc20Ref8x8;

fn spk(s: &AnyState) -> String {
    spk_to_string(&s.spk())
}

/// A deterministic xorshift generator.
struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    /// `total` cut into `k` positive parts.
    fn split(&mut self, total: i64, k: usize) -> Vec<i64> {
        let mut cuts: Vec<i64> = (0..k - 1).map(|_| 1 + self.below(total as u64 - 1) as i64).collect();
        cuts.push(0);
        cuts.push(total);
        cuts.sort();
        cuts.dedup();
        cuts.windows(2).map(|w| w[1] - w[0]).collect()
    }
}

fn taker_tokens(n: i64) -> Vec<TokenUtxo> {
    vec![tok(21, n, pk(TAKER), SCHEME_P2PK, 1_000)]
}

fn batch(legs: Vec<Leg>, taker: Vec<TokenUtxo>) -> Batch {
    let mut b = route(legs);
    b.taker_tokens = taker;
    b
}

// ---------------------------------------------------------------- (a) the rounding rule

/// Split fills of an ask never pay its maker less than one fill of the whole amount, and of a bid never charge its maker
/// more: over random splits (every split fill validated in the engine), the payouts the covenant pins are the ceil /
/// floor of each fill's exact value, their sum is at least (ask) / at most (bid) the single fill's, and splitting moves
/// less than one sompi per fill to the maker.
#[test]
fn split_fills_never_pay_a_seller_less_or_charge_a_buyer_more_than_one_fill() {
    let mut g = Rng(0x5eed_1234_abcd_0001);
    let total = 10 * WHOLE;
    let a0 = AskState { price: 250_000_007, tip: 13, min_fill: 1, ..ask(MAKER_A, 0, P) };
    let b0 = BidState { price: 245_000_003, tip: 7, min_fill: 1, ..bid(MAKER_B, 0, P) };
    let ask_single = a0.proceeds(total, 0, 0).unwrap();
    let bid_single = b0.spend(total, 0, 0).unwrap();
    for trial in 0..3 {
        let k = 2 + trial;
        let parts = g.split(total, k);
        // the ask: each fill pays exactly ceil(n * (price - tip) / scale) at output 0 (plus the carriers when it sells out)
        let mut cur = a0.clone();
        let mut paid = 0i64;
        for (i, n) in parts.iter().copied().enumerate() {
            let c = cov(0xa1);
            let leg = Leg::Ask {
                order: order(10, CARRIER, c, 1_000, cur.clone()),
                custody: tok(11, cur.amount_left, c, SCHEME_COVID, 1_000),
                amount: n,
                t: None,
            };
            let label = format!("ask split {trial} fill {i} ({n} of {})", cur.amount_left);
            let (_, signed) = run_any(&label, &Action::Batch(batch(vec![leg], vec![])));
            let last = n == cur.amount_left;
            let proceeds = signed.tx.outputs[0].value as i64 - if last { 2 * CARRIER as i64 } else { 0 };
            assert_eq!(proceeds, cur.proceeds(n, 0, 0).unwrap(), "{label}");
            assert!(proceeds as i128 * SCALE as i128 >= n as i128 * (a0.price - a0.tip) as i128, "{label}: at least the exact value");
            paid += proceeds;
            cur.amount_left -= n;
        }
        assert!(paid >= ask_single && paid - ask_single < parts.len() as i64, "ask {parts:?}: {paid} vs {ask_single}");
        // the bid: each fill costs its maker floor(n * (price + tip) / scale): escrow in = continuation + delivery + spend
        let s = b0.clone();
        let mut v = b0.escrow(total, parts.len() as i64).unwrap();
        let mut spent = 0i64;
        for (i, n) in parts.iter().copied().enumerate() {
            let leg = Leg::Bid { order: order(20, v as u64, cov(0xb1), 1_000, s.clone()), amount: n, t: None };
            let label = format!("bid split {trial} fill {i} ({n})");
            let (_, signed) = run_any(&label, &Action::Batch(batch(vec![leg], taker_tokens(n))));
            let cont =
                signed.tx.outputs.iter().find(|o| o.script_public_key == spk(&AnyState::KobBid(s.clone()))).map(|o| o.value as i64);
            let delivery = signed.tx.outputs[0].value as i64;
            let spend = v - cont.unwrap_or(0) - delivery;
            assert_eq!(spend, s.spend(n, 0, 0).unwrap(), "{label}");
            assert!(spend as i128 * SCALE as i128 <= n as i128 * (b0.price + b0.tip) as i128, "{label}: at most the exact value");
            spent += spend;
            match cont {
                Some(c) => v = c,
                None => assert_eq!(i + 1, parts.len(), "{label}: the escrow funds every split of the amount"),
            }
        }
        assert!(spent <= bid_single && bid_single - spent < parts.len() as i64, "bid {parts:?}: {spent} vs {bid_single}");
    }
}

// ---------------------------------------------------------------- (b) the minimum fill of every kind

/// One minimum-fill case: `mk(state)` lays out a fill of `n` of the order `state` (leg 0, input 0); `lo` accepts it
/// (minFill = n), `hi` refuses it (minFill = n + 1, and n does not take everything left); `cont(state)` is the
/// continuation the fill leaves (`None`: the fill terminates the order).
fn min_fill_case(
    label: &str,
    lo: AnyState,
    hi: AnyState,
    mk: impl Fn(&AnyState) -> Action,
    cont: impl Fn(&AnyState) -> Option<AnyState>,
) {
    // the builder refuses the fill below the minimum
    let e = build_measured(&mk(&hi)).expect_err(label).to_string();
    assert!(e.contains("minFill"), "{label}: {e}");
    // the covenant: the same transaction with the order's state at minFill = n passes, at n + 1 fails at the order input
    let built = build_measured(&mk(&lo)).unwrap_or_else(|e| panic!("{label}: build: {e}"));
    let ok = patched_outcomes(built.clone(), 0, lo.encode(), spk(&lo), &[], &[]);
    assert!(ok.iter().all(|r| r.is_ok()), "{label}: control {ok:?}");
    let outs: Vec<(String, String)> = match (cont(&lo), cont(&hi)) {
        (Some(a), Some(b)) => vec![(spk(&a), spk(&b))],
        _ => vec![],
    };
    // the continuation the patch rewrites is really there (so the order input fails on its minFill, nothing else)
    for (from, _) in &outs {
        assert!(built.tx.outputs.iter().any(|o| o.script_public_key == *from), "{label}: no continuation output");
    }
    let bad = patched_outcomes(built, 0, hi.encode(), spk(&hi), &outs, &[]);
    assert!(bad[0].is_err(), "{label}: the covenant accepted a fill below its minFill: {bad:?}");
    assert!(bad[1..].iter().all(|r| r.is_ok()), "{label}: only the order input fails: {bad:?}");
}

#[test]
fn the_minimum_fill_of_every_kind_is_the_covenants() {
    let n = 1_500;
    let rest = 10 * WHOLE - n;

    // ask: a GTC partial fill
    let ask_of = |m: i64| AnyState::KobAsk(AskState { min_fill: m, ..ask(MAKER_A, P250, P) });
    min_fill_case(
        "ask",
        ask_of(n),
        ask_of(n + 1),
        |s| {
            let AnyState::KobAsk(a) = s else { unreachable!() };
            let c = cov(0xa1);
            let leg =
                Leg::Ask { order: order(10, CARRIER, c, 1_000, a.clone()), custody: custody(11, 10, c, 1_000), amount: n, t: None };
            Action::Batch(batch(vec![leg], vec![]))
        },
        |s| {
            let AnyState::KobAsk(a) = s else { unreachable!() };
            Some(AnyState::KobAsk(AskState { amount_left: rest, ..a.clone() }))
        },
    );

    // bid: a GTC fill that leaves buying power for more (so the bid continues: the minimum applies)
    let bid_of = |m: i64| AnyState::KobBid(BidState { min_fill: m, ..bid(MAKER_B, P245, P) });
    let bv = |s: &BidState| s.escrow(10 * WHOLE, 3).unwrap() as u64;
    min_fill_case(
        "bid",
        bid_of(n),
        bid_of(n + 1),
        |s| {
            let AnyState::KobBid(b) = s else { unreachable!() };
            let leg = Leg::Bid { order: order(20, bv(b), cov(0xb1), 1_000, b.clone()), amount: n, t: None };
            Action::Batch(batch(vec![leg], taker_tokens(n)))
        },
        |s| Some(s.clone()),
    );
    // ... and the exception: a fill that leaves less than one minimum fill of buying power terminates the bid, at any size
    let AnyState::KobBid(hi) = bid_of(n + 1) else { unreachable!() };
    let small = (hi.used(n).unwrap() + hi.delivery_carrier) as u64;
    let leg = Leg::Bid { order: order(20, small, cov(0xb1), 1_000, hi.clone()), amount: n, t: None };
    let (built, _) = run_any("bid: the terminating fill below minFill", &Action::Batch(batch(vec![leg], taker_tokens(n))));
    assert!(built.roles[0].contains(".fill.close"), "{:?}", built.roles);

    // conditional ask: a take-profit partial fill
    let ca = |m: i64| AnyState::KobCondAsk(CondAskState { min_fill: m, ..cond_ask(MAKER_A, P) });
    min_fill_case(
        "condAsk",
        ca(n),
        ca(n + 1),
        |s| {
            let AnyState::KobCondAsk(c) = s else { unreachable!() };
            let id = cov(0xc1);
            let leg = Leg::CondAsk {
                order: order(30, CARRIER, id, 2_000, c.clone()),
                custody: custody(31, 10, id, 2_000),
                amount: n,
                leg: 0,
                evidence: None,
                t: None,
                merge: None,
            };
            Action::Batch(batch(vec![leg], vec![]))
        },
        |s| {
            let AnyState::KobCondAsk(c) = s else { unreachable!() };
            Some(AnyState::KobCondAsk(CondAskState { amount_left: rest, ..c.clone() }))
        },
    );

    // conditional bid: a limit-leg partial fill
    let cb = |m: i64| AnyState::KobCondBid(CondBidState { min_fill: m, ..cond_bid(MAKER_B, P) });
    min_fill_case(
        "condBid",
        cb(n),
        cb(n + 1),
        |s| {
            let AnyState::KobCondBid(c) = s else { unreachable!() };
            let v = c.escrow(2).unwrap() as u64;
            let leg = Leg::CondBid {
                order: order(40, v, cov(0xe1), 2_000, c.clone()),
                amount: n,
                leg: 0,
                evidence: None,
                t: None,
                merge: None,
            };
            Action::Batch(batch(vec![leg], taker_tokens(n)))
        },
        |s| {
            let AnyState::KobCondBid(c) = s else { unreachable!() };
            Some(AnyState::KobCondBid(CondBidState { amount_left: rest, ..c.clone() }))
        },
    );

    // buy-first entry: a partial fill (creates its exit, the entry continues)
    let ib = |m: i64| AnyState::KobIfdBid(IfdBidState { min_fill: m, ..ifd_bid(MAKER_A, 10, P) });
    let AnyState::KobIfdBid(ib_lo) = ib(n) else { unreachable!() };
    let ib_value = ib_lo.escrow().unwrap() as u64;
    min_fill_case(
        "ifdBid",
        ib(n),
        ib(n + 1),
        |s| {
            let AnyState::KobIfdBid(i) = s else { unreachable!() };
            let leg = Leg::IfdBid { order: order(50, ib_value, cov(0xd1), 1_000, i.clone()), amount: n, evidence: None, t: None };
            Action::Batch(batch(vec![leg], taker_tokens(n)))
        },
        |s| {
            let AnyState::KobIfdBid(i) = s else { unreachable!() };
            Some(AnyState::KobIfdBid(IfdBidState { amount_left: rest, ..i.clone() }))
        },
    );

    // sell-first entry: a partial fill
    let ia = |m: i64| AnyState::KobIfdAsk(IfdAskState { min_fill: m, ..ifd_ask(MAKER_A, P) });
    let AnyState::KobIfdAsk(ia_lo) = ia(n) else { unreachable!() };
    let ia_value = ia_lo.escrow(CARRIER as i64).unwrap() as u64;
    min_fill_case(
        "ifdAsk",
        ia(n),
        ia(n + 1),
        |s| {
            let AnyState::KobIfdAsk(i) = s else { unreachable!() };
            let c = cov(0xf1);
            let leg = Leg::IfdAsk {
                order: order(60, ia_value, c, 1_000, i.clone()),
                custody: custody(61, 10, c, 1_000),
                amount: n,
                evidence: None,
                t: None,
            };
            Action::Batch(batch(vec![leg], vec![]))
        },
        |s| {
            let AnyState::KobIfdAsk(i) = s else { unreachable!() };
            Some(AnyState::KobIfdAsk(IfdAskState { amount_left: rest, ..i.clone() }))
        },
    );

    // pair ask: a GTC partial fill through a bid of A and an ask of B (the B surplus rides on the maker's delivery)
    let xs = |m: i64| AnyState::KobPair(PairState { min_fill: m, ..pair(MAKER_A, true, P, P, RATE) });
    min_fill_case(
        "pair",
        xs(n),
        xs(n + 1),
        |s| {
            let AnyState::KobPair(x) = s else { unreachable!() };
            let bs = BidState { min_fill: n, ..bid(MAKER_B, P260, P) };
            let bv = bs.escrow(n, 1).unwrap() as u64;
            Action::Batch(route(vec![
                pair_leg_amount(x.clone(), P, P, X_ID, 70, n),
                Leg::Bid { order: order(72, bv, cov(0x91), 1_000, bs), amount: n, t: None },
                kask_leg(TOKEN_B, P, MAKER_C, P250, cov(0x92), 74, 10, 2),
            ]))
        },
        |s| {
            let AnyState::KobPair(x) = s else { unreachable!() };
            Some(AnyState::KobPair(PairState { amount_left: rest, custody: rest, ..x.clone() }))
        },
    );
}

// ---------------------------------------------------------------- (c) overflow limits

/// The covenant's quote fails closed exactly where `quote_of` returns `None`, and a fill at the largest amount whose quote
/// still fits validates.
///
/// * scale 1 (the result overflow): an ask of price 2^40 sells out `n` base units; `n x 2^40` fits up to n = 2^23 - 1.
///   The fill is built at price 1 and patched to the real price (and the payout output to the value the covenant then
///   demands: the scripts only, a payout near 2^63 sompi cannot be funded).
/// * a scale beyond the split's exactness bound (4 x 10^9, refused by the builders' gate at placement but not by the
///   covenant): an intermediate product overflows although the result fits; the largest fitting amount validates as a
///   real, funded transaction, one more base unit fails.
#[test]
fn the_covenant_quote_fails_closed_exactly_where_quote_of_does() {
    // scale 1, price 2^40: the result boundary
    let p40 = 1i64 << 40;
    let fits = i64::MAX / p40; // 2^23 - 1
    for (n, ok) in [(fits, true), (fits + 1, false)] {
        assert_eq!(quote_of(n, p40, 1, Round::Up).is_some(), ok);
        let real = AskState { scale: 1, min_fill: 1, tip: 0, price: p40, amount_left: n, ..ask(MAKER_A, P250, P) };
        let cheap = AskState { price: 1, ..real.clone() };
        let c = cov(0xa1);
        let leg =
            Leg::Ask { order: order(10, CARRIER, c, 1_000, cheap), custody: tok(11, n, c, SCHEME_COVID, 1_000), amount: n, t: None };
        let built = build_measured(&Action::Batch(batch(vec![leg], vec![]))).unwrap();
        let demand = if ok { (n as u64) * (p40 as u64) + 2 * CARRIER } else { u64::MAX / 4 };
        let r = AnyState::KobAsk(real);
        let out = patched_outcomes(built, 0, r.encode(), spk(&r), &[], &[(0, demand)]);
        assert_eq!(out[0].is_ok(), ok, "n = {n}: {:?}", out[0]);
    }
    // the builder refuses the overflowing fill itself
    let big = AskState { scale: 1, min_fill: 1, tip: 0, price: p40, amount_left: fits + 1, ..ask(MAKER_A, P250, P) };
    let c = cov(0xa1);
    let leg = Leg::Ask {
        order: order(10, CARRIER, c, 1_000, big),
        custody: tok(11, fits + 1, c, SCHEME_COVID, 1_000),
        amount: fits + 1,
        t: None,
    };
    assert!(build_measured(&Action::Batch(batch(vec![leg], vec![]))).is_err());

    // scale 4 x 10^9: m * (rate % scale) + scale - 1 overflows from m = (2^63 - scale) / (scale - 1) + 1 on
    let scale = 4_000_000_000i64;
    let rate = scale - 1; // 0.999.. sompi per base unit: the result fits in an i64 by far
    let m_max = (i64::MAX - (scale - 1)) / rate;
    assert!(quote_of(m_max, rate, scale, Round::Up).is_some());
    assert!(quote_of(m_max + 1, rate, scale, Round::Up).is_none());
    assert!(quote_exact(m_max + 1, rate, scale, Round::Up).unwrap() < 10_000_000_000, "the result itself is small");
    for (n, ok) in [(m_max, true), (m_max + 1, false)] {
        let real = AskState { scale, min_fill: 1, tip: 0, price: rate, amount_left: n, ..ask(MAKER_A, P250, P) };
        let leg = Leg::Ask {
            order: order(10, CARRIER, c, 1_000, real.clone()),
            custody: tok(11, n, c, SCHEME_COVID, 1_000),
            amount: n,
            t: None,
        };
        let a = Action::Batch(batch(vec![leg], vec![]));
        if ok {
            run_any("the largest fitting amount at scale 4e9", &a);
        } else {
            assert!(build_measured(&a).is_err(), "the builder refuses where the covenant fails");
            // the covenant: built with a fitting state (rate 1: the same outputs but for the payout)
            let fit = AskState { price: 1, ..real.clone() };
            let leg =
                Leg::Ask { order: order(10, CARRIER, c, 1_000, fit), custody: tok(11, n, c, SCHEME_COVID, 1_000), amount: n, t: None };
            let built = build_measured(&Action::Batch(batch(vec![leg], vec![]))).unwrap();
            let r = AnyState::KobAsk(real);
            let out = patched_outcomes(built, 0, r.encode(), spk(&r), &[], &[(0, 1_000_000 * KAS)]);
            assert!(out[0].is_err(), "n = {n}: the covenant's split overflows: {:?}", out[0]);
        }
    }
}

// ---------------------------------------------------------------- (d) the repeat merge argument

/// `-(k x 2^53 + m)` as an 8-byte script number: the covenants split it as k = v / 2^53, m = v % 2^53. The largest amount
/// (2^53 - 1) and the exit indices up to 999 (and the bound 1023) round-trip; beyond them the builder refuses.
#[test]
fn the_merge_argument_round_trips_at_its_limits() {
    let decode = |b: &[u8]| {
        assert_eq!(b.len(), 8);
        assert_eq!(b[7] & 0x80, 0x80, "negative");
        let mut m = [0u8; 8];
        m.copy_from_slice(b);
        m[7] &= 0x7f;
        let v = i64::from_le_bytes(m);
        (v / MERGE_SHIFT, v % MERGE_SHIFT)
    };
    let m_max = MERGE_SHIFT - 1;
    for k in [0usize, 1, 7, 255, 256, 998, 999, 1023] {
        for m in [1i64, 2, 1 << 40, m_max] {
            let b = merge_arg(k, m).unwrap();
            assert_eq!(decode(&b), (k as i64, m), "k {k} m {m}");
        }
    }
    assert!(merge_arg(1024, 1).is_err());
    assert!(merge_arg(0, MERGE_SHIFT).is_err());
    assert!(merge_arg(0, 0).is_err());
    assert_eq!(MERGE_SHIFT, 1 << 53);
}

/// A booked exit of 2^53 - 1 base units (the largest amount a booked exit may hold) takes profit on all of it and merges
/// into its empty repeating entry through the engine: the merge argument carries m = 2^53 - 1 at k = 0, the entry takes
/// ceil(m x (price + tip) / scale) back.
#[test]
fn a_merge_of_the_largest_booked_amount_validates() {
    let scale = 1_000_000_000i64;
    let m = MERGE_SHIFT - 1;
    let exit0 = CondAskState { scale, min_fill: 1, ..ifd_exit(MAKER_A, P) };
    let entry = IfdBidState {
        scale,
        min_fill: 1,
        amount_left: 0,
        rpt_amount: 1,
        exit_state: IfdBidState::commit_exit(&exit0),
        ..ifd_bid(MAKER_A, 10, P)
    };
    let exit = entry.exit_for(m, Some(Booking { parent: cov(0xd1), until: booked_until() })).unwrap();
    assert!(entry.books_exit(cov(0xd1), &exit));
    let leg = Leg::CondAsk {
        order: order(30, CARRIER, cov(0xc1), 2_000, exit.clone()),
        custody: tok(31, m, cov(0xc1), SCHEME_COVID, 2_000),
        amount: m,
        leg: 0,
        evidence: None,
        t: None,
        merge: Some(order(50, 20 * KAS, cov(0xd1), 1_000, entry.clone())),
    };
    let mut b = batch(vec![leg], vec![]);
    b.funding = vec![key_utxo(33, TAKER, 100_000_000 * KAS)];
    b.taker = Some(pk(TAKER));
    b.change = Some(pk(TAKER));
    let (built, signed) = run_any("merge at m = 2^53 - 1", &Action::Batch(b));
    let SigPlan::Entry { args, .. } = &built.plans[1] else { panic!("the merged entry is input 1") };
    assert_eq!(args[0], Arg::Bytes(merge_arg(0, m).unwrap()));
    let budget = entry.merge_budget(m).unwrap();
    let cont = IfdBidState { amount_left: m, armed: 0, ..entry.clone() };
    let o = signed.tx.outputs.iter().find(|o| o.script_public_key == spk(&AnyState::KobIfdBid(cont.clone()))).expect("entry");
    // the exit sells out: its carriers ride back to the entry with the budget
    assert_eq!(o.value as i64, 20 * KAS as i64 + budget + 2 * CARRIER as i64);
    assert_eq!(signed.tx.outputs[0].value as i64, exit.proceeds(m, exit.tp_price).unwrap() - budget);
}
