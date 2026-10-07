use super::*;
use silverscript_abi::ArtifactValue;

/// The stop band multiplies first, like the covenants; rounding keeps the maker's side; a stop above MAX_STOP_PRICE
/// saturates.
#[test]
fn stop_band_multiplies_first() {
    assert_eq!(band(9_999, 300), 299);
    assert_eq!(band(100, 300), 3);
    assert_eq!(band(200_000_000, 300), 6_000_000);
    assert_eq!(band(12_345, 1), 1);
    assert_eq!(band(MAX_STOP_PRICE, 10_000), MAX_STOP_PRICE);
    assert_eq!(band(MAX_STOP_PRICE + 1, 10_000), i64::MAX);
}

// ---------------------------------------------------------------- quoteOf

/// The covenant's split formula, written out with checked i64 operations in the covenant's order (an independent copy of
/// [`quote_of`] for the tests).
fn covenant_split(n: i64, r: i64, scale: i64, c: i64) -> Option<i64> {
    let m = n % scale;
    let q = n / scale;
    let t1 = q.checked_mul(r)?;
    let t2 = m.checked_mul(r / scale)?;
    let t3 = m.checked_mul(r % scale)?.checked_add(c)? / scale;
    t1.checked_add(t2)?.checked_add(t3)
}

fn exact(n: i64, r: i64, scale: i64, round: Round) -> i128 {
    let (num, s) = (n as i128 * r as i128, scale as i128);
    match round {
        Round::Down => num.div_euclid(s),
        Round::Up => (num + s - 1).div_euclid(s),
    }
}

/// A deterministic xorshift generator (no dependency).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    /// A value spread over every magnitude up to `max` (log-uniform bit length).
    fn wide(&mut self, max: i64) -> i64 {
        let bits = self.below(64) as u32;
        let v = (self.next() >> (64 - bits.max(1))) as i64;
        v.min(max)
    }
}

/// Brute force over small scales, amounts and rates: floor and ceil of n * r / scale, and the covenant split.
#[test]
fn quote_of_is_floor_and_ceil_of_the_exact_quotient() {
    for scale in [1i64, 2, 3, 7, 10, 100, 1_000] {
        for n in 0..300i64 {
            for r in (0..2_000i64).step_by(7) {
                let down = quote_of(n, r, scale, Round::Down).unwrap();
                let up = quote_of(n, r, scale, Round::Up).unwrap();
                assert_eq!(down as i128, exact(n, r, scale, Round::Down), "{n} {r} {scale}");
                assert_eq!(up as i128, exact(n, r, scale, Round::Up), "{n} {r} {scale}");
                assert_eq!(Some(down), covenant_split(n, r, scale, 0));
                assert_eq!(Some(up), covenant_split(n, r, scale, scale - 1));
                assert!(up - down <= 1);
            }
        }
    }
    // invalid inputs: the covenants never pass them
    assert_eq!(quote_of(-1, 5, 10, Round::Up), None);
    assert_eq!(quote_of(5, -1, 10, Round::Up), None);
    assert_eq!(quote_of(5, 5, 0, Round::Down), None);
    assert_eq!(quote_of(5, 5, -10, Round::Down), None);
}

/// The limits: scale 1, 10^9 and 3,037,000,498; amounts and rates up to i64::MAX; results just below and above 2^63; a
/// product that overflows u64 but whose result fits.
#[test]
fn quote_of_limits() {
    let max = i64::MAX;
    for scale in [1i64, 1_000_000_000, 3_037_000_498] {
        // the result is the only failure mode: Some iff the exact result fits in an i64
        for (n, r) in [(max, 1), (1, max), (max, scale), (max / scale, scale), (max, scale - 1), (scale - 1, max), (max, max)] {
            for round in [Round::Down, Round::Up] {
                let e = exact(n, r, scale, round);
                let got = quote_of(n, r, scale, round);
                assert_eq!(got.map(|x| x as i128), (e <= max as i128).then_some(e), "{n} {r} {scale} {round:?}");
            }
        }
    }
    // a product far beyond u64 (n * r ~ 2^125) whose result fits: exact
    let (n, r, s) = (max, 1_000_000_000, 1_000_000_000);
    assert_eq!(quote_of(n, r, s, Round::Down), Some(max));
    assert_eq!(quote_of(n, r, s, Round::Up), Some(max));
    let (n, r) = (6_000_000_000_000_000_000i64, 1_500_000_000i64); // 9e27 / 1e9 = 9e18 < 2^63
    assert_eq!(quote_of(n, r, s, Round::Down), Some(9_000_000_000_000_000_000));
    // just below and just above 2^63: scale 1 and the largest amount
    assert_eq!(quote_of(max, 1, 1, Round::Up), Some(max));
    assert_eq!(quote_of(max, 2, 1, Round::Up), None);
    // ceil of a quotient right at the top: (2^63 - 1) * 3 / 3 = 2^63 - 1 exactly, (2^63 - 1) * 3 / 2 does not fit
    assert_eq!(quote_of(max, 3, 3, Round::Up), Some(max));
    assert_eq!(quote_of(max, 3, 2, Round::Down), None);
    // floor fits while ceil does not: n * r = 2^64 - 1 = (2^63 - 1) * 2 + 1, scale 2
    let (n, r) = ((1i64 << 32) - 1, (1i64 << 32) + 1);
    assert_eq!(quote_of(n, r, 2, Round::Down), Some(max));
    assert_eq!(quote_of(n, r, 2, Round::Up), None);
    assert_eq!(covenant_split(n, r, 2, 1), None);
    // 10^9 scale, rate i64::MAX: n up to scale - 1 keeps the result below the rate
    assert_eq!(
        quote_of(999_999_999, max, 1_000_000_000, Round::Up).map(|x| x as i128),
        Some(exact(999_999_999, max, 1_000_000_000, Round::Up))
    );
    // beyond the split's exactness bound (scale^2 + scale >= 2^63) the covenant fails although the result fits: the
    // mirror fails with it
    let big = 4_000_000_000i64;
    let (n, r) = (big - 1, big - 1);
    assert!(exact(n, r, big, Round::Up) < max as i128);
    assert_eq!(quote_of(n, r, big, Round::Up), None);
    assert_eq!(covenant_split(n, r, big, big - 1), None);
}

/// Random amounts and rates of every magnitude: the mirror equals the covenant split, and equals the exact result
/// whenever that fits (scale <= 10^9).
#[test]
fn quote_of_random_magnitudes() {
    let mut g = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..200_000 {
        let scale = 10i64.pow(g.below(10) as u32);
        let (n, r) = (g.wide(i64::MAX), g.wide(i64::MAX));
        for (round, c) in [(Round::Down, 0), (Round::Up, scale - 1)] {
            let got = quote_of(n, r, scale, round);
            assert_eq!(got, covenant_split(n, r, scale, c));
            let e = exact(n, r, scale, round);
            assert_eq!(got.map(|x| x as i128), (e <= i64::MAX as i128).then_some(e), "{n} {r} {scale}");
        }
    }
}

/// The rounding rule: however an order is split into fills, every fill pays a seller at least (Up) and charges a buyer at
/// most (Down) the exact value of its amount, and the sum over the fills is at least / at most the value of the total.
#[test]
fn split_fills_never_pay_a_seller_less_or_charge_a_buyer_more() {
    let mut g = Rng(0x1234_5678_9abc_def1);
    for _ in 0..20_000 {
        let scale = 10i64.pow(g.below(10) as u32);
        let total = 1 + g.wide(1 << 31);
        let rate = g.wide(1 << 31);
        let k = 1 + g.below(12) as usize;
        let mut cuts: Vec<i64> = (0..k - 1).map(|_| g.below(total as u64) as i64).collect();
        cuts.push(0);
        cuts.push(total);
        cuts.sort();
        let parts: Vec<i64> = cuts.windows(2).map(|w| w[1] - w[0]).collect();
        let (mut up, mut down) = (0i128, 0i128);
        for &p in &parts {
            let u = quote_of(p, rate, scale, Round::Up).unwrap() as i128;
            let d = quote_of(p, rate, scale, Round::Down).unwrap() as i128;
            let x = p as i128 * rate as i128; // exact value times scale
            assert!(u * scale as i128 >= x && d * scale as i128 <= x);
            up += u;
            down += d;
        }
        let whole_up = quote_of(total, rate, scale, Round::Up).unwrap() as i128;
        let whole_down = quote_of(total, rate, scale, Round::Down).unwrap() as i128;
        assert!(up >= whole_up, "ceil is superadditive");
        assert!(down <= whole_down, "floor is subadditive");
        // splitting moves less than one quote unit per fill to the maker
        assert!(up - whole_up < parts.len() as i128 && whole_down - down < parts.len() as i128);
    }
}

#[test]
fn scale_and_quote_gate() {
    for s in [1, 10, 100, 1_000_000, 1_000_000_000] {
        check_scale(s).unwrap();
    }
    for s in [0, -10, 2, 20_000_001, 10_000_000_000, 999_999_999] {
        assert!(check_scale(s).is_err(), "{s}");
    }
    check_quote(1 << 41, 1 << 21, 1, "x").unwrap_err(); // exactly 2^62
    check_quote((1 << 41) - 1, 1 << 21, 1, "x").unwrap();
    check_quote(i64::MAX, 1, 1_000_000_000, "x").unwrap();
    check_quote(i64::MAX, 500_000_000, 1_000_000_000, "x").unwrap_err();
}

fn bid() -> BidState {
    let t = template(TemplateId::KobBid);
    BidState::decode(&t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap()
}

/// The bid's escrow arithmetic: used (ceil, budget rate), spend (floor, the quote), buying power, the minimum-fill rule.
#[test]
fn bid_budget_rules() {
    let b = BidState {
        scale: 1_000,
        min_fill: 300,
        price: 1_001,
        tip: 10,
        reserve: 7,
        delivery_carrier: 100,
        slope: 0,
        max_fill: 0,
        ..bid()
    };
    assert_eq!(b.used(1), Some(2)); // ceil(1011 / 1000)
    assert_eq!(b.spend_at(1, 1_001), Some(1)); // floor
    assert_eq!(b.used(1_000), Some(1_011));
    // buying power: the largest n with used(n) <= value - carrier - reserve
    let value = 100 + 7 + 1_011;
    assert_eq!(b.buying_power(value), 1_000);
    assert_eq!(b.buying_power(value - 1), 999);
    assert!(b.used(b.buying_power(value - 1) + 1).unwrap() > value - 1 - 107);
    // the minimum fill: n >= minFill unless the bid cannot continue after the fill
    assert!(b.fill_ok(300, value));
    assert!(!b.fill_ok(299, value)); // leaves 1011 - 303 = 708 >= used(300) = 304: it continues, so too small
    let small = 107 + b.used(400).unwrap();
    assert!(b.fill_ok(200, small)); // leaves less than one minimum fill: a terminating fill of any size
    assert!(!b.fill_ok(0, value));
    // a terminating fill may use the budget down to the reserve: used(n) <= value - reserve = 1111, n <= 1098
    assert!(b.fill_ok(1_098, value) && !b.fill_ok(1_099, value));
    // escrow for an amount in k fills funds every split of it
    let e = b.escrow(1_000, 4).unwrap();
    assert_eq!(e, 1_011 + 3 + 4 * 100 + 7);
}

// ---------------------------------------------------------------- pair orders

fn compiled_pair() -> PairState {
    let t = template(TemplateId::KobPair);
    PairState::decode(&t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap()
}

/// A pair order's fill amounts: an ask receives the ceil of B, a bid pays exactly the floor from its escrow; the tip is
/// the floor of KAS; decay moves an ask down to priceEnd and a bid up to it; a partial fill needs the KAS of its delivery
/// and tip and a positive rest; FOK fills completely.
#[test]
fn pair_fill_amounts_round_for_the_maker() {
    let base = PairState {
        side: SIDE_ASK,
        s_scale: 1_000,
        t_scale: 100,
        price: 2_501,
        tip: 7,
        min_fill: 1,
        max_fill: 0,
        interval: 0,
        slope: 0,
        tif: TIF_GTC,
        delivery_carrier: 1_000,
        amount_left: 10_000,
        custody: 10_000,
        active_from: 0,
        ..compiled_pair()
    };
    let f = base.fill(333, base.price, 10_000).unwrap();
    assert_eq!((f.s_out, f.t_out, f.tip_kas, f.rest, f.out_amount), (333, 833, 2, true, 9_667));
    // an ask's custody is its whole amount
    assert!(PairState { custody: 9_999, ..base.clone() }.fill(1, 2_501, 10_000).is_err());
    // the KAS of a partial fill
    assert!(base.fill(333, base.price, 1_001).is_err());
    assert!(base.fill(10_000, base.price, 0).is_ok(), "the last fill takes the order's KAS anyway");
    // a bid: B escrow pays exactly floor(n * p / scale(A)), A delivered exactly
    let bid = PairState { side: SIDE_BID, s_scale: 100, t_scale: 1_000, custody: 2_600, ..base.clone() };
    let g = bid.fill(333, 2_501, 10_000).unwrap();
    assert_eq!((g.s_out, g.t_out, g.tip_kas, g.out_amount), (832, 333, 2, 2_600 - 832));
    assert!(PairState { custody: 832, ..bid.clone() }.fill(333, 2_501, 10_000).is_err(), "a rest must keep something");
    assert!(PairState { custody: 831, tif: TIF_IOC, ..bid.clone() }.fill(333, 2_501, 10_000).is_err(), "the escrow pays");
    assert!(PairState { tif: TIF_FOK, ..bid.clone() }.fill(333, 2_501, 10_000).is_err());
    // decay: ask down, bid up, both bounded by priceEnd
    let d = PairState { slope: 10, decay_step: 5, price_end: 2_401, active_from: 100, ..base.clone() };
    assert_eq!(d.price_at(100, 0), Some(2_501));
    assert_eq!(d.price_at(149, 0), Some(2_411));
    assert_eq!(d.price_at(1_000, 0), Some(2_401));
    let r = PairState { side: SIDE_BID, slope: 10, decay_step: 5, price_end: 2_601, active_from: 100, ..bid.clone() };
    assert_eq!(r.price_at(149, 0), Some(2_591));
    assert_eq!(r.price_at(1_000, 0), Some(2_601));
    assert_eq!(r.price_max(), 2_601);
    // the bid escrow helper funds any split: the floor of the whole amount at the highest price plus one unit
    let e = r.bid_escrow(10_000, 4).unwrap();
    assert_eq!(e, 10_000 * 2_601 / 1_000 + 1);
    // takeability: the largest takeable amount
    assert_eq!(base.max_takeable(base.price, 10_000), 10_000);
    let capped = PairState { max_fill: 400, ..base.clone() };
    assert_eq!(capped.max_takeable(capped.price, 10_000), 400);
    let poor = PairState { max_fill: 400, ..base.clone() };
    assert_eq!(poor.max_takeable(poor.price, 999), 0, "no KAS for a partial fill, and the cap forbids the whole amount");
}

/// The numeric gate of the pair kinds: both scales powers of ten up to 10^9, full fills below 2^62 at every rate, a KRON
/// custody or amount at most 10^9 base units.
#[test]
fn pair_numeric_gate() {
    let p = PairState { s_scale: 100_000_000, t_scale: 100_000_000, amount_left: 1_000, custody: 1_000, price: 5, ..compiled_pair() };
    AnyState::KobPair(p.clone()).check_numbers().unwrap();
    assert!(AnyState::KobPair(PairState { t_scale: 300, ..p.clone() }).check_numbers().is_err());
    assert!(AnyState::KobPair(PairState { s_scale: 1, amount_left: 1 << 40, custody: 1 << 40, price: 1 << 22, ..p.clone() })
        .check_numbers()
        .is_err());
    let kron = PairState { s_family: 2, amount_left: 2_000_000_000, custody: 2_000_000_000, ..p.clone() };
    assert!(AnyState::KobPair(kron.clone()).check_numbers().is_err(), "a KRON custody above 10^9");
    assert!(AnyState::KobPair(PairState { s_family: 1, ..kron }).check_numbers().is_ok());
}

// ---------------------------------------------------------------- codec

/// A typed state of template `id` decoded WITHOUT `AnyState::validate` (the compiled example instances carry placeholder
/// constructor values, e.g. an if-done entry committing an exit at another scale).
fn typed(id: TemplateId, b: &[u8]) -> AnyState {
    match id.base() {
        TemplateId::KobAsk => AnyState::KobAsk(AskState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobBid => AnyState::KobBid(BidState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobCondAsk => AnyState::KobCondAsk(CondAskState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobCondBid => AnyState::KobCondBid(CondBidState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobIfdBid => AnyState::KobIfdBid(IfdBidState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobIfdAsk => AnyState::KobIfdAsk(IfdAskState::decode_as(id, b).unwrap()).into_family(id.family()),
        TemplateId::KobPair => AnyState::KobPair(PairState::decode_as(id, b).unwrap()),
        TemplateId::KobCondPair => AnyState::KobCondPair(CondPairState::decode_as(id, b).unwrap()),
        TemplateId::KobIfdPair => AnyState::KobIfdPair(IfdPairState::decode_as(id, b).unwrap()),
        other => panic!("{} is not an order template", other.name()),
    }
}

/// Every committed artifact was compiled from its ctor file; decoding its state span with the
/// typed codec and re-encoding must give the same bytes.
#[test]
fn typed_states_roundtrip_committed_artifacts() {
    for id in TemplateId::ALL {
        if !id.is_artifact() {
            // A raw KRON program: its first 46 bytes are a state span.
            let tt = crate::artifacts::token_template(id);
            let st = KronState::decode(
                &[0x20]
                    .into_iter()
                    .chain([7u8; 32])
                    .chain([0x01, 3, 0x08])
                    .chain(5i64.to_le_bytes())
                    .chain([0x01, 0])
                    .collect::<Vec<u8>>(),
            )
            .unwrap();
            assert_eq!(st.encode().len(), tt.state_len);
            assert_eq!(TokenState::from_redeem_with(tt, &st.redeem_with(tt)).unwrap(), TokenState::Kron(st));
            continue;
        }
        let t = template(id);
        let bc = &t.contract().compiled.bytecode;
        let span = &bc[t.prefix.len()..t.prefix.len() + t.state_len];
        if id.is_token() {
            let s = Kcc20State::decode(span).unwrap();
            assert_eq!(s.encode(), span, "{}", id.name());
            assert_eq!(s.redeem_with(t), *bc);
        } else {
            let s = typed(id, span);
            // the compiled example instances of the if-done entries commit an exit at another scale (placeholder ctor
            // values): the state decoder (AnyState::validate) refuses exactly those
            let entry = matches!(id.base(), TemplateId::KobIfdBid | TemplateId::KobIfdAsk);
            assert_eq!(AnyState::decode(id, span).is_ok(), !entry || s.validate().is_ok(), "{}", id.name());
            assert_eq!(s.encode(), span, "{}", id.name());
            assert_eq!(s.redeem(), *bc);
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(serde_json::from_str::<AnyState>(&json).unwrap(), s);
        }
    }
}

#[test]
fn ctor_files_match_typed_states() {
    let ctor: Vec<ArtifactValue> = serde_json::from_str(include_str!("../../../../contracts/v2/KobAsk.ctor.json")).unwrap();
    let t = template(TemplateId::KobAsk);
    let s = AskState::from_redeem(&t.contract().compiled.bytecode).unwrap();
    assert_eq!(ArtifactValue::Bytes(s.maker.to_vec()), ctor[0]);
    assert_eq!(ArtifactValue::Int(s.scale), ctor[5]);
    assert_eq!(ArtifactValue::Int(s.min_fill), ctor[6]);
    assert_eq!(ArtifactValue::Int(s.price), ctor[7]);
    assert_eq!(ArtifactValue::Int(s.expiry_daa), ctor[11]);
    assert_eq!(ArtifactValue::Int(s.decay_step), ctor[17]);
    assert_eq!(ArtifactValue::Int(s.amount_left), ctor[18]);
    // The hand-coded offsets the covenants read (the touch evidence of KobCondAsk & co.) match the encoding.
    let st = s.encode();
    assert_eq!(&st[34..66], &s.token_cov_id);
    assert_eq!(i64::from_le_bytes(st[118..126].try_into().unwrap()), s.scale);
    assert_eq!(i64::from_le_bytes(st[136..144].try_into().unwrap()), s.price);
    // The committed exit prefixes end right before the repeat fields.
    let ib = IfdBidState::decode(&template(TemplateId::KobIfdBid).contract().compiled.bytecode[1..1 + 585]).unwrap();
    assert_eq!(ib.exit_state.len(), IFD_BID_EXIT_COMMIT);
    let ca = CondAskState::decode(&template(TemplateId::KobCondAsk).contract().compiled.bytecode[1..1 + 330]).unwrap();
    let ib = IfdBidState { exit_state: IfdBidState::commit_exit(&ca), ..ib };
    assert_eq!(ib.exit().unwrap(), ca);
    let b = Booking { parent: [9; 32], until: 77 };
    let x = ib.exit_for(3, Some(b)).unwrap();
    assert_eq!((x.amount_left, x.parent, x.rpt_price, x.rpt_until), (3, [9; 32], ib.price + ib.tip, 77));
    let ia = IfdAskState::decode(&template(TemplateId::KobIfdAsk).contract().compiled.bytecode[1..1 + 594]).unwrap();
    assert_eq!(ia.exit_state.len(), IFD_ASK_EXIT_COMMIT);
    let cb = CondBidState::decode(&template(TemplateId::KobCondBid).contract().compiled.bytecode[1..1 + 381]).unwrap();
    let ia = IfdAskState { exit_state: IfdAskState::commit_exit(&cb), ..ia };
    assert_eq!(ia.exit().unwrap(), cb);
    let x = ia.exit_for(2, Some(Booking { parent: [9; 32], until: 5 })).unwrap();
    assert_eq!((x.amount_left, x.rpt_price, x.rpt_pre, x.rpt_until), (2, ia.price - ia.tip, ia.prefund, 5));
}

/// The splice windows published in `docs/spec/matcher.md` §1.1 are exactly where each mutable
/// field (and the repeat fields) sits in the redeem script.
#[test]
fn mutable_windows_match_the_encoding() {
    for id in [
        TemplateId::KobAsk,
        TemplateId::KobCondAsk,
        TemplateId::KobCondBid,
        TemplateId::KobIfdBid,
        TemplateId::KobIfdAsk,
        TemplateId::KobAskKron,
        TemplateId::KobCondAskKron,
        TemplateId::KobCondBidKron,
        TemplateId::KobIfdBidKron,
        TemplateId::KobIfdAskKron,
        TemplateId::KobPair,
        TemplateId::KobCondPair,
        TemplateId::KobIfdPair,
    ] {
        let t = template(id);
        let base = typed(id, &t.contract().compiled.bytecode[1..1 + t.state_len]);
        let base_rs = base.redeem();
        assert!(!mutable_windows(id).is_empty(), "{}", id.name());
        for (k, (field, a, b)) in mutable_windows(id).iter().enumerate() {
            let v = 0x0102_0304_0506_0000 + k as i64;
            let json = serde_json::to_value(&base).unwrap();
            let mut j = json.clone();
            j["state"][*field] = serde_json::Value::String(v.to_string());
            let changed: AnyState = serde_json::from_value(j).unwrap();
            let rs = changed.redeem();
            assert_eq!(&rs[*a..*b], &v.to_le_bytes(), "{} {field}", id.name());
            assert_eq!(&rs[..*a], &base_rs[..*a], "{} {field}", id.name());
            assert_eq!(&rs[*b..], &base_rs[*b..], "{} {field}", id.name());
        }
        if let Some((a, b)) = repeat_window(id) {
            let json = serde_json::to_value(&base).unwrap();
            let mut j = json.clone();
            j["state"]["parent"] = serde_json::Value::String("ab".repeat(32));
            let rs = serde_json::from_value::<AnyState>(j).unwrap().redeem();
            assert_eq!(&rs[a + 1..a + 33], &[0xab; 32], "{}", id.name());
            // the repeat fields end the state; a KobCondPair's sExt (0x20 + 32 B) follows them
            let tail = if id == TemplateId::KobCondPair { 33 } else { 0 };
            assert_eq!(b + tail, 1 + t.state_len, "{}: the repeat fields end the state", id.name());
        }
    }
}

/// KRON: same fields as KCC-20 except the bid-side kinds have no extension commitment; the state
/// codec drops it (and refuses a non-zero one), the committed exit of a KRON sell-first entry is 288 B.
#[test]
fn kron_states_have_no_extension_commitment() {
    for (kcc, kron) in [
        (TemplateId::KobBid, TemplateId::KobBidKron),
        (TemplateId::KobCondBid, TemplateId::KobCondBidKron),
        (TemplateId::KobIfdBid, TemplateId::KobIfdBidKron),
    ] {
        let tk = template(kron);
        let span = &tk.contract().compiled.bytecode[1..1 + tk.state_len];
        assert_eq!(tk.state_len + 33, template(kcc).state_len, "{}", kron.name());
        let st = typed(kron, span);
        assert_eq!(st.extension_commitment(), Some([0; 32]));
        assert_eq!(st.encode(), span);
        let as_kcc = st.clone().into_family(Family::Kcc20);
        assert_eq!(as_kcc.template_id(), kcc);
        assert_eq!(as_kcc.encode().len(), template(kcc).state_len);
        let mut j = serde_json::to_value(&st).unwrap();
        j["state"]["extensionCommitment"] = serde_json::Value::String("ee".repeat(32));
        let bad: AnyState = serde_json::from_value(j).unwrap();
        assert!(bad.validate().is_err(), "{}", kron.name());
        assert!(encode_state_json(&serde_json::to_string(&bad).unwrap()).is_err());
    }
    let ia = IfdAskState::decode_as(TemplateId::KobIfdAskKron, &{
        let t = template(TemplateId::KobIfdAskKron);
        t.contract().compiled.bytecode[1..1 + t.state_len].to_vec()
    })
    .unwrap();
    assert_eq!((ia.exit_state.len(), ia.exit_family().unwrap()), (IFD_ASK_EXIT_COMMIT_KRON, Family::Kron));
    let tc = template(TemplateId::KobCondBidKron);
    let cb = CondBidState::decode_as(TemplateId::KobCondBidKron, &tc.contract().compiled.bytecode[1..1 + tc.state_len]).unwrap();
    let ia = IfdAskState { exit_state: IfdAskState::commit_exit_for(Family::Kron, &cb), ..ia };
    assert_eq!(ia.exit().unwrap(), cb);
    assert_eq!(ia.exit_state.len(), IFD_ASK_EXIT_COMMIT_KRON);
    assert!(AnyState::KobIfdAsk(ia).validate().is_err());
}

/// The pair kinds are one template each for both families: the family of an instance is its base token A's, `in_family`
/// keeps the template, an invalid family code or A == B is refused.
#[test]
fn one_pair_template_for_both_families() {
    for id in [TemplateId::KobPair, TemplateId::KobCondPair, TemplateId::KobIfdPair] {
        assert_eq!(id.in_family(Family::Kron), id);
        assert!(id.serves(Family::Kron) && id.serves(Family::Kcc20));
        assert_eq!(TemplateId::from_kind_code(Family::Kron, id.kind_code().unwrap()), Some(id));
    }
    assert_eq!(TemplateId::from_kind_code(Family::Kcc20, 0x08), Some(TemplateId::KobPair));
    assert!(!TemplateId::KobAsk.serves(Family::Kron));
    let x = compiled_pair();
    for (side, s_code, t_code, fam) in
        [(1, 1, 2, Family::Kcc20), (1, 2, 1, Family::Kron), (2, 1, 2, Family::Kron), (2, 2, 1, Family::Kcc20)]
    {
        let s = AnyState::KobPair(PairState { side, s_family: s_code, t_family: t_code, t_ext: [0; 32], s_ext: [0; 32], ..x.clone() });
        assert_eq!(s.family(), fam);
        assert_eq!(s.clone().into_family(Family::Kcc20), s);
        s.validate().unwrap();
    }
    assert!(AnyState::KobPair(PairState { s_family: 3, ..x.clone() }).validate().is_err());
    assert!(AnyState::KobPair(PairState { side: 3, ..x.clone() }).validate().is_err());
    assert!(AnyState::KobPair(PairState { t_cov_id: x.s_cov_id, ..x.clone() }).validate().is_err());
    assert!(AnyState::KobPair(PairState { t_family: 2, ..x }).validate().is_err(), "a KRON T with an extension commitment");
}

#[test]
fn kron_token_state_codec() {
    let s = KronState { owner: [9; 32], id_type: 2, amount: 1_234_567, is_minter: 0 };
    let b = s.encode();
    assert_eq!(b.len(), KRON_STATE_LEN);
    assert_eq!((b[0], b[33], b[34], b[35], b[44], b[45]), (0x20, 0x01, 2, 0x08, 0x01, 0));
    assert_eq!(&b[1..33], &[9; 32]);
    assert_eq!(&b[36..44], &1_234_567i64.to_le_bytes());
    assert_eq!(KronState::decode(&b).unwrap(), s);
    let mut bad = b.clone();
    bad[33] = 0x02;
    assert!(KronState::decode(&bad).is_err());
    assert!(KronState::decode(&b[..45]).is_err());
    let t: TokenState = serde_json::from_str(&serde_json::to_string(&TokenState::Kron(s.clone())).unwrap()).unwrap();
    assert_eq!(t, TokenState::Kron(s));
    let k = Kcc20State::p2pk(5, [1; 32], [2; 32]);
    let t: TokenState = serde_json::from_str(&serde_json::to_string(&TokenState::Kcc20(k.clone())).unwrap()).unwrap();
    assert_eq!(t, TokenState::Kcc20(k));
    let (user, custody) =
        (TokenState::user(Family::Kron, 3, [4; 32], [0; 32]), TokenState::custody(Family::Kron, 3, [4; 32], [0; 32]));
    assert!(user.is_user() && !user.is_covenant_owned() && custody.is_covenant_owned() && !custody.is_user());
}

#[test]
fn non_canonical_state_is_rejected() {
    let t = template(TemplateId::KobAsk);
    let mut span = t.contract().compiled.bytecode[1..1 + t.state_len].to_vec();
    span[0] = 0x21; // wrong push length
    assert!(AskState::decode(&span).is_err());
}

// ---------------------------------------------------------------- checked covenant arithmetic

/// Decay, rise, stop band, entry auction, refund time and rptUntil mirror the covenants' checked arithmetic: where a
/// covenant would fail (an overflow), the helper says so (`None`, or "never" for a refund time) instead of panicking or
/// wrapping, so a hostile order is unfillable, not a crash.
#[test]
fn covenant_arithmetic_is_checked_at_the_limits() {
    // decay / rise: slope * steps and price -/+ moved
    assert_eq!(decay_down(1_000, 900, 10, 5, 100, 149), Some(910));
    assert_eq!(decay_down(1_000, 0, i64::MAX, 1, 0, 2), None, "slope * steps overflows");
    assert_eq!(decay_down(i64::MIN + 5, 0, 10, 1, 0, 1), None, "price - moved overflows");
    assert_eq!(decay_down(1_000, 0, 1, 0, 0, 1), None, "decayStep 0");
    assert_eq!(decay_down(1_000, 0, 1, 1, i64::MIN, i64::MAX), None, "t - origin overflows");
    assert_eq!(rise_up(1_000, 2_000, 10, 5, 100, 149), Some(1_090));
    assert_eq!(rise_up(i64::MAX - 5, i64::MAX, 10, 1, 0, 1), None, "price + moved overflows");
    assert_eq!(decay_origin(5, i64::MAX, 1), None, "UTXO DAA + interval overflows");
    let a = AskState {
        slope: i64::MAX,
        decay_step: 1,
        active_from: 0,
        interval: 0,
        ..AskState::decode(&{
            let t = template(TemplateId::KobAsk);
            t.contract().compiled.bytecode[1..1 + t.state_len].to_vec()
        })
        .unwrap()
    };
    assert_eq!(a.price_at(10, 0), None);
    assert_eq!(a.proceeds(1, 10, 0), None);
    // stop band: the auction's basis points and the band itself
    assert_eq!(band_bps(300, 300, 0, 150), Some(150));
    assert_eq!(band_bps(i64::MAX, i64::MAX, 0, 3), None, "slipBps * e overflows");
    assert_eq!(band_bps(300, 300, i64::MIN, 1), None);
    assert_eq!(stop_band(MAX_STOP_PRICE, 10_000), Some(MAX_STOP_PRICE));
    assert_eq!(stop_band(MAX_STOP_PRICE + 1, 1), None, "a stop leg above MAX_STOP_PRICE is refused");
    // entry auction: (price - entryStop) * e
    let ib = IfdBidState::decode(&{
        let t = template(TemplateId::KobIfdBid);
        t.contract().compiled.bytecode[1..1 + t.state_len].to_vec()
    })
    .unwrap();
    let ib = IfdBidState { entry_stop: 1, price: i64::MAX, band_daa: i64::MAX, armed: 2, ..ib };
    assert_eq!(ib.price_at(false, 4, 0), None);
    // refund time and rptUntil
    assert_eq!(refund_due(10, TIF_IOC, i64::MAX, 0), i64::MAX, "the kill time overflows: never refundable");
    assert_eq!(refund_due(10, TIF_GTC, 0, i64::MAX), i64::MAX);
    assert_eq!(refund_due(10, TIF_GTC, 0, 5), 10);
    assert_eq!(rpt_until(i64::MAX, i64::MAX, 0), None);
    assert_eq!(rpt_until(10, 0, 0), Some(10));
}
