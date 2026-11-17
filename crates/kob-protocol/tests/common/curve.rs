//! `KobCurveAsk` fixtures (`docs/design/curve-ask-spec.md`): the curve, a taker's fills, a matcher cross with a bid
//! of each delivery mode, the cancel, the amend and the creation.

use super::*;

/// The curve's covenant id in the fixtures.
pub const CURVE_COV: [u8; 32] = [0xc5; 32];

/// HIP-2: `p_0 = start`, `p_i = round(p_{i-1} · 1.003)`.
pub fn hip2(start: i64, levels: usize) -> Vec<i64> {
    let mut v = vec![start];
    while v.len() < levels {
        let p = *v.last().unwrap();
        v.push((p * 1003 + 500) / 1000);
    }
    v
}

/// A curve of maker A on `tpl`: `levels` levels of one whole token each, priced from 2.50 KAS on the HIP-2 grid.
pub fn curve(tpl: TemplateId, levels: usize) -> CurveAskState {
    let (h, p, s) = tok_fields(tpl);
    let total = levels as i64 * WHOLE;
    CurveAskState {
        maker: pk(MAKER_A),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: WHOLE,
        total,
        amount_left: total,
        extension_commitment: EXT,
        table: hip2(P250, levels),
    }
}

/// The curve leg: the order (cov [`CURVE_COV`]) and its custody of exactly `amountLeft`.
pub fn curve_leg(s: &CurveAskState, n: i64) -> Leg {
    Leg::CurveAsk {
        order: order(40, carrier(), CURVE_COV, 1_000, s.clone()),
        custody: tok(41, s.amount_left, CURVE_COV, SCHEME_COVID, 1_000),
        amount: n,
    }
}

/// A taker buys `n` base units.
pub fn take_curve(s: &CurveAskState, n: i64) -> Batch {
    let mut b = batch(vec![curve_leg(s, n)]);
    b.funding = vec![key_utxo(42, TAKER, 10_000 * KAS)];
    b
}

/// A matcher crosses a bid of maker B (at 2.70 KAS + tip, funded for `n`, delivery `dlv`) with the curve.
pub fn cross_curve(s: &CurveAskState, n: i64, bs: BidState, dlv: Option<BidDelivery>) -> Batch {
    let v = bs.escrow(n, 1).expect("escrow") as u64;
    let mut b = batch(vec![Leg::Bid { order: order(43, v, cov(0xb4), 1_000, bs), amount: n, t: None, delivery: dlv }, curve_leg(s, n)]);
    b.change = Some(pk(MATCHER));
    b.funding = vec![key_utxo(44, MATCHER, 10 * KAS)];
    b
}

/// The bid of a cross: maker B at 2.70 KAS in delivery `mode`.
pub fn cross_bid(tpl: TemplateId, mode: i64, bought: i64) -> BidState {
    mode_bid(BidState { min_fill: 1, ..bid(MAKER_B, 270_000_000, tpl) }, mode, bought)
}

/// The maker's cancel: the custody back to the maker.
pub fn cancel_curve(s: &CurveAskState) -> CancelOrder {
    CancelOrder {
        order: order(40, carrier(), CURVE_COV, 1_000, AnyState::KobCurveAsk(s.clone())),
        retired: None,
        custody: Some(tok(41, s.amount_left, CURVE_COV, SCHEME_COVID, 1_000)),
        prefund: None,
        strays: vec![],
        foreign: vec![],
        tokens: vec![],
        funding: vec![key_utxo(45, MAKER_A, 5 * KAS)],
        change: None,
        replace: None,
        lock_time: NOW,
        records: vec![],
        fee: fee(),
    }
}

/// The maker amends in place: a new table (+10%) and tip.
pub fn amend_curve(s: &CurveAskState) -> AmendOrder {
    let amended = CurveAskState { table: s.table.iter().map(|p| p * 11 / 10).collect(), tip: TIP, ..s.clone() };
    AmendOrder {
        order: order(40, carrier(), CURVE_COV, 1_000, AnyState::KobCurveAsk(s.clone())),
        amended: AnyState::KobCurveAsk(amended),
        value: None,
        funding: vec![key_utxo(45, MAKER_A, 5 * KAS)],
        change: None,
        lock_time: NOW,
        deadline: None,
        records: vec![],
        fee: fee(),
    }
}

/// The maker places the curve from the tokens it holds.
pub fn create_curve(tpl: TemplateId, s: &CurveAskState) -> CreateOrder {
    CreateOrder {
        order: AnyState::KobCurveAsk(s.clone()),
        value: carrier(),
        tokens: vec![tok_on(tpl, 46, s.amount_left + 7 * WHOLE, pk(MAKER_A), 1_000)],
        token_carrier: carrier(),
        funding: vec![key_utxo(47, MAKER_A, 100 * KAS)],
        change: None,
        lock_time: NOW,
        deadline: None,
        records: vec![],
        fee: fee(),
    }
}

/// Every curve shape on `tpl` (name, action): placement, fills inside one level and across 8, a market buy, the
/// sell-out, crosses with bids of modes 0, 1 (first and later fill) and 2 (form 1), the cancel and the amend.
pub fn curve_scenarios(tpl: TemplateId, levels: usize) -> Vec<(String, Action)> {
    let c = curve(tpl, levels);
    let mid = CurveAskState { amount_left: c.total - WHOLE / 2, ..c.clone() };
    let near_end = CurveAskState { amount_left: 3 * WHOLE + 1, ..c.clone() };
    let budget = mid.max_for_budget(20 * KAS as i64);
    let mut v = vec![
        ("create.curve".to_string(), Action::CreateOrder(create_curve(tpl, &c))),
        ("take.curve.level".into(), Action::Batch(take_curve(&c, WHOLE / 3))),
        ("take.curve.cross8".into(), Action::Batch(take_curve(&mid, 7 * WHOLE + WHOLE / 4))),
        ("take.curve.market".into(), Action::Batch(take_curve(&mid, budget))),
        ("take.curve.close".into(), Action::Batch(take_curve(&near_end, near_end.amount_left))),
        ("cross.curve.m0".into(), Action::Batch(cross_curve(&mid, 3 * WHOLE, cross_bid(tpl, MODE_NEW, 0), None))),
        ("cross.curve.m1".into(), Action::Batch(cross_curve(&mid, 3 * WHOLE, cross_bid(tpl, MODE_ACCUMULATE, 0), None))),
        (
            "cross.curve.m1c".into(),
            Action::Batch(cross_curve(
                &mid,
                3 * WHOLE,
                cross_bid(tpl, MODE_ACCUMULATE, 2 * WHOLE),
                bid_dlv(Some(bought_custody(48, 2 * WHOLE, cov(0xb4), 1_000)), None, None),
            )),
        ),
        ("cancel.curve".into(), Action::CancelOrder(cancel_curve(&mid))),
        ("amend.curve".into(), Action::AmendOrder(amend_curve(&mid))),
    ];
    if kob_protocol::budget::topup_extra(tpl).is_some() {
        v.push(("cross.curve.m2".into(), Action::Batch(cross_curve(&mid, 3 * WHOLE, cross_bid(tpl, MODE_TOPUP, 0), None))));
    }
    v
}
