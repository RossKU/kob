//! The delivery block of the bid-side and pair kinds (`docs/design/bid-delivery-spec.md` §2): both
//! state layouts, their codec, golden vectors and the delivery rules.
//!
//! The layouts do not depend on which one the pinned artifact has: the legacy layout is that of the
//! templates before the block (the retired ones once the block is pinned), the delivery layout that
//! of the templates with it. Every test here runs on both.

use super::*;

/// The spec table (§2): template, state length today, the block's state offsets, state length after,
/// and the redeem window of `bought` (none for an entry).
type SpecRow = (TemplateId, usize, [usize; 4], usize, Option<(usize, usize)>);
/// The spec table, one row per delivering template.
const SPEC: [SpecRow; 6] = [
    (TemplateId::KobBid, 285, [285, 294, 303, 312], 321, Some((314, 322))),
    (TemplateId::KobCondBid, 381, [381, 390, 399, 408], 417, Some((410, 418))),
    (TemplateId::KobPair, 447, [447, 456, 465, 474], 483, Some((476, 484))),
    (TemplateId::KobCondPair, 543, [543, 552, 561, 570], 579, Some((572, 580))),
    (TemplateId::KobIfdAsk, 594, [594, 603, 612, 0], 621, None),
    (TemplateId::KobIfdPair, 909, [909, 918, 927, 0], 936, None),
];

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
    /// Any i64, spread over every magnitude and both signs.
    fn any_i64(&mut self) -> i64 {
        let bits = self.below(64) as u32;
        let v = (self.next() >> (64 - bits.max(1))) as i64;
        if self.below(4) == 0 {
            v.wrapping_neg()
        } else {
            v
        }
    }
}

/// The typed state of a span, decoded without `AnyState::validate` (the compiled example instances
/// carry placeholder values), and its layout.
fn typed_layout(id: TemplateId, b: &[u8]) -> Result<(AnyState, StateLayout), StateError> {
    Ok(match id.base() {
        TemplateId::KobAsk => {
            let (s, l) = AskState::decode_layout(id, b)?;
            (AnyState::KobAsk(s).into_family(id.family()), l)
        }
        TemplateId::KobBid => {
            let (s, l) = BidState::decode_layout(id, b)?;
            (AnyState::KobBid(s).into_family(id.family()), l)
        }
        TemplateId::KobCondAsk => {
            let (s, l) = CondAskState::decode_layout(id, b)?;
            (AnyState::KobCondAsk(s).into_family(id.family()), l)
        }
        TemplateId::KobCondBid => {
            let (s, l) = CondBidState::decode_layout(id, b)?;
            (AnyState::KobCondBid(s).into_family(id.family()), l)
        }
        TemplateId::KobIfdBid => {
            let (s, l) = IfdBidState::decode_layout(id, b)?;
            (AnyState::KobIfdBid(s).into_family(id.family()), l)
        }
        TemplateId::KobIfdAsk => {
            let (s, l) = IfdAskState::decode_layout(id, b)?;
            (AnyState::KobIfdAsk(s).into_family(id.family()), l)
        }
        TemplateId::KobPair => {
            let (s, l) = PairState::decode_layout(id, b)?;
            (AnyState::KobPair(s), l)
        }
        TemplateId::KobCondPair => {
            let (s, l) = CondPairState::decode_layout(id, b)?;
            (AnyState::KobCondPair(s), l)
        }
        TemplateId::KobIfdPair => {
            let (s, l) = IfdPairState::decode_layout(id, b)?;
            (AnyState::KobIfdPair(s), l)
        }
        other => panic!("{} is not an order template", other.name()),
    })
}

/// The compiled example instance of `id` (whichever layout its artifact has).
///
/// An entry's placeholder committed exit is replaced by the compiled exit of its family, so that
/// its exits decode.
fn compiled(id: TemplateId) -> AnyState {
    let t = template(id);
    let s = typed_layout(id, &t.contract().compiled.bytecode[1..1 + t.state_len]).unwrap().0;
    let exit = |x: TemplateId| compiled(x);
    match s {
        AnyState::KobIfdAsk(e) | AnyState::KobIfdAskKron(e) => {
            let fam = id.family();
            let (AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x)) = exit(TemplateId::KobCondBid.in_family(fam)) else {
                unreachable!()
            };
            AnyState::KobIfdAsk(IfdAskState { exit_state: IfdAskState::commit_exit_for(fam, &x), ..e }).into_family(fam)
        }
        AnyState::KobIfdPair(e) => {
            let AnyState::KobCondPair(x) = exit(TemplateId::KobCondPair) else { unreachable!() };
            AnyState::KobIfdPair(IfdPairState { exit_state: IfdPairState::commit_exit(&x), ..e })
        }
        other => other,
    }
}

/// The state with its own or exit delivery block set to `d` (an entry ignores `d.bought`).
fn with_block(s: &AnyState, d: Delivery) -> AnyState {
    let mut s = s.clone();
    match &mut s {
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => {
            (x.mode, x.recv_threshold, x.topup_tip, x.bought) = (d.mode, d.recv_threshold, d.topup_tip, d.bought)
        }
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => {
            (x.mode, x.recv_threshold, x.topup_tip, x.bought) = (d.mode, d.recv_threshold, d.topup_tip, d.bought)
        }
        AnyState::KobPair(x) => (x.mode, x.recv_threshold, x.topup_tip, x.bought) = (d.mode, d.recv_threshold, d.topup_tip, d.bought),
        AnyState::KobCondPair(x) => {
            (x.mode, x.recv_threshold, x.topup_tip, x.bought) = (d.mode, d.recv_threshold, d.topup_tip, d.bought)
        }
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => {
            (x.exit_mode, x.exit_recv_threshold, x.exit_topup_tip) = (d.mode, d.recv_threshold, d.topup_tip)
        }
        AnyState::KobIfdPair(x) => (x.exit_mode, x.exit_recv_threshold, x.exit_topup_tip) = (d.mode, d.recv_threshold, d.topup_tip),
        _ => {}
    }
    s
}

/// The block's values in state order, as the span spells them.
fn block_values(id: TemplateId, d: Delivery) -> Vec<i64> {
    let all = [d.mode, d.recv_threshold, d.topup_tip, d.bought];
    all[..delivery_block(id).unwrap().fields.len()].to_vec()
}

fn int_pushes(values: &[i64]) -> Vec<u8> {
    values.iter().flat_map(|v| std::iter::once(0x08u8).chain(v.to_le_bytes())).collect()
}

fn any_block(r: &mut Rng) -> Delivery {
    Delivery { mode: r.any_i64(), recv_threshold: r.any_i64(), topup_tip: r.any_i64(), bought: r.any_i64() }
}

/// §2: the block sits where the spec table says, in both layouts, and the pinned artifact has one
/// of the two state lengths.
#[test]
fn the_block_is_where_the_spec_table_says() {
    for (id, legacy, offsets, after, bought) in SPEC {
        let b = delivery_block(id).unwrap_or_else(|| panic!("{} has a block", id.name()));
        assert_eq!((b.legacy_len, b.len()), (legacy, after), "{}", id.name());
        assert_eq!(b.fields.len(), if bought.is_some() { 4 } else { 3 }, "{}", id.name());
        for (k, f) in b.fields.iter().enumerate() {
            assert_eq!(b.offset(f), Some(offsets[k]), "{} {f}", id.name());
            assert_eq!(b.window(f), Some((offsets[k] + 2, offsets[k] + 10)), "{} {f}", id.name());
        }
        assert_eq!(b.window("bought"), bought, "{}", id.name());
        assert_eq!((b.layout_of_len(legacy), b.layout_of_len(after)), (Some(StateLayout::Legacy), Some(StateLayout::Delivery)));
        for n in [0, legacy - 1, legacy + 1, after - 1, after + 1, after + 9] {
            assert_eq!(b.layout_of_len(n), None, "{} {n}", id.name());
        }
        // the pinned template has one of the two (P1 moves it from the first to the second)
        let pinned = pinned_layout(id);
        assert_eq!(template(id).state_len, b.state_len(pinned), "{}", id.name());
        assert_eq!(template(id).prefix.len(), 1, "{}: redeem = 1 byte prefix ‖ state ‖ code", id.name());
        assert_eq!(state_len_in(id, StateLayout::Legacy), Some(legacy));
        assert_eq!(state_len_in(id, StateLayout::Delivery), Some(after));
    }
    // no block: the asks, the buy-first entry and every KRON twin (a KRON delivery is mode 0)
    for id in [
        TemplateId::KobAsk,
        TemplateId::KobCondAsk,
        TemplateId::KobIfdBid,
        TemplateId::KobAskKron,
        TemplateId::KobBidKron,
        TemplateId::KobCondAskKron,
        TemplateId::KobCondBidKron,
        TemplateId::KobIfdBidKron,
        TemplateId::KobIfdAskKron,
        TemplateId::KobBidKronD0,
        TemplateId::KobCondBidKronD0,
        TemplateId::KobIfdAskKronD0,
    ] {
        assert_eq!(delivery_block(id), None, "{}", id.name());
        assert_eq!(pinned_layout(id), StateLayout::Legacy);
        assert_eq!(state_len_in(id, StateLayout::Delivery), None);
        assert_eq!(layout_of_len(id, template(id).state_len), Some(StateLayout::Legacy));
    }
}

/// Fuzz: any block values round-trip in the delivery layout; the span is the legacy span of the
/// same terms followed by the block's `0x08` pushes, so every existing field keeps its offset; the
/// legacy span decodes with the block zero (mode 0).
#[test]
fn both_layouts_round_trip_and_the_block_is_appended() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for (id, ..) in SPEC {
        let base = compiled(id);
        let zero = with_block(&base, Delivery::NEW);
        let legacy = zero.try_encode_layout(StateLayout::Legacy).unwrap();
        let b = delivery_block(id).unwrap();
        assert_eq!(legacy.len(), b.legacy_len, "{}", id.name());
        for _ in 0..300 {
            let d = any_block(&mut r);
            let s = with_block(&base, d);
            let span = s.try_encode_layout(StateLayout::Delivery).unwrap();
            assert_eq!(span.len(), b.len(), "{}", id.name());
            assert_eq!(&span[..b.legacy_len], &legacy[..], "{}: the existing fields do not move", id.name());
            assert_eq!(&span[b.legacy_len..], &int_pushes(&block_values(id, d))[..], "{}", id.name());
            let (back, layout) = typed_layout(id, &span).unwrap();
            assert_eq!((back, layout), (s.clone(), StateLayout::Delivery), "{}", id.name());
            // the JSON keeps the block
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(serde_json::from_str::<AnyState>(&json).unwrap(), s);
        }
        let (back, layout) = typed_layout(id, &legacy).unwrap();
        assert_eq!((back.clone(), layout), (zero.clone(), StateLayout::Legacy), "{}", id.name());
        assert_eq!(back.block_fields(), Some(Delivery::NEW));
        // zero in the delivery layout is the legacy span plus zero pushes
        assert_eq!(
            zero.try_encode_layout(StateLayout::Delivery).unwrap(),
            [legacy.clone(), int_pushes(&block_values(id, Delivery::NEW))].concat()
        );
    }
}

/// Fuzz: a span of neither length, a block field that is not an 8-byte push, or a block appended
/// twice does not decode.
#[test]
fn malformed_block_spans_are_refused() {
    let mut r = Rng(0x2545_f491_4f6c_dd1d);
    for (id, ..) in SPEC {
        let b = delivery_block(id).unwrap();
        let s = with_block(&compiled(id), any_block(&mut r));
        let span = s.try_encode_layout(StateLayout::Delivery).unwrap();
        // truncated or extended by up to two pushes
        for cut in 1..=2 * INT_PUSH {
            assert!(typed_layout(id, &span[..span.len() - cut]).is_err() || span.len() - cut == b.legacy_len, "{} -{cut}", id.name());
            let mut longer = span.clone();
            longer.extend(vec![0x08u8; cut]);
            assert!(typed_layout(id, &longer).is_err(), "{} +{cut}", id.name());
        }
        // every block opcode is 0x08 and nothing else
        for k in 0..b.fields.len() {
            let at = b.legacy_len + INT_PUSH * k;
            for _ in 0..20 {
                let op = r.below(256) as u8;
                if op == 0x08 {
                    continue;
                }
                let mut bad = span.clone();
                bad[at] = op;
                assert!(typed_layout(id, &bad).is_err(), "{} {} opcode {op:#04x}", id.name(), b.fields[k]);
            }
        }
        // random lengths between and around the two layouts
        for _ in 0..50 {
            let n = b.legacy_len - 20 + r.below(b.len() as u64 - b.legacy_len as u64 + 40) as usize;
            if n == b.legacy_len || n == b.len() {
                continue;
            }
            let mut v = span.clone();
            v.resize(n, 0x08);
            assert!(typed_layout(id, &v).is_err(), "{} length {n}", id.name());
        }
    }
}

/// The legacy layout has no block: a non-zero block field is not encodable there (it would be a
/// mode the legacy script ignores), and `validate_layout` says so. Ablation: without the check the
/// legacy encoding would silently drop the mode.
#[test]
fn the_legacy_layout_carries_no_delivery_fields() {
    for (id, ..) in SPEC {
        let base = with_block(&compiled(id), Delivery::NEW);
        assert!(base.try_encode_layout(StateLayout::Legacy).is_ok());
        for k in 0..delivery_block(id).unwrap().fields.len() {
            let mut d = [0i64; 4];
            d[k] = 1;
            let s = with_block(&base, Delivery { mode: d[0], recv_threshold: d[1], topup_tip: d[2], bought: d[3] });
            let e = s.try_encode_layout(StateLayout::Legacy).unwrap_err().to_string();
            assert!(e.contains("must be 0"), "{}: {e}", id.name());
            let e = s.validate_layout(StateLayout::Legacy).unwrap_err().to_string();
            assert!(e.contains("legacy layout has no delivery block"), "{}: {e}", id.name());
            assert!(s.try_encode_layout(StateLayout::Delivery).is_ok());
        }
    }
}

/// A KRON twin has no block: its delivery fields decode as zero and must be zero to encode; a kind
/// without a block has no delivery layout at all.
#[test]
fn kron_twins_and_kinds_without_a_block_stay_mode_zero() {
    for id in [TemplateId::KobBidKron, TemplateId::KobCondBidKron, TemplateId::KobIfdAskKron, TemplateId::KobBidKronD0] {
        let s = compiled(id);
        assert_eq!(s.block_fields(), Some(Delivery::NEW), "{}", id.name());
        let m = with_block(&s, Delivery { mode: 1, ..Delivery::NEW });
        let e = m.try_encode().unwrap_err().to_string();
        assert!(e.contains("no delivery block"), "{}: {e}", id.name());
        assert!(m.validate().is_err(), "{}", id.name());
        assert!(s.try_encode_layout(StateLayout::Delivery).is_err(), "{}", id.name());
        assert!(s.validate_layout(StateLayout::Delivery).is_err(), "{}", id.name());
        assert_eq!(s.try_encode().unwrap().len(), template(id).state_len);
    }
    for id in [TemplateId::KobAsk, TemplateId::KobCondAsk, TemplateId::KobIfdBid] {
        let s = compiled(id);
        assert_eq!(s.block_fields(), None);
        assert!(s.try_encode_layout(StateLayout::Delivery).is_err(), "{}", id.name());
        assert_eq!(s.try_encode_layout(StateLayout::Legacy).unwrap(), s.try_encode().unwrap());
    }
}

/// `AnyState::decode` accepts both layouts of a delivering kind (a retired order keeps the legacy
/// one) and tells which; `decode_kind` and the JSON without the block (absent = 0) agree.
#[test]
fn any_state_decodes_either_layout() {
    // valid states (AnyState::decode validates): a plain bid and a pair order of two tokens
    // the compiled example bid and pair order are valid states (`AnyState::decode` validates)
    for s in [compiled(TemplateId::KobBid), compiled(TemplateId::KobPair)] {
        let id = s.template_id();
        let d = with_block(&s, Delivery { mode: 1, recv_threshold: 0, topup_tip: 0, bought: 42 });
        let span = d.try_encode_layout(StateLayout::Delivery).unwrap();
        assert_eq!(AnyState::decode_layout(id, &span).unwrap(), (d.clone(), StateLayout::Delivery));
        assert_eq!(AnyState::decode_kind(id, &span).unwrap(), d);
        let legacy = s.try_encode_layout(StateLayout::Legacy).unwrap();
        let zero = with_block(&s, Delivery::NEW);
        assert_eq!(AnyState::decode_layout(id, &legacy).unwrap(), (zero.clone(), StateLayout::Legacy));
        // a JSON state without the block reads as mode 0
        let mut j = serde_json::to_value(&d).unwrap();
        for k in DELIVERY_FIELDS {
            assert!(j["state"].as_object_mut().unwrap().remove(k).is_some());
        }
        assert_eq!(serde_json::from_value::<AnyState>(j).unwrap(), zero);
    }
}

/// The entries' exits (§2 *Exit states*): `KobIfdAsk` → the `KobCondBid` exit as built today ‖
/// `exitMode` ‖ `exitRecvThreshold` ‖ `exitTopupTip` ‖ `bought = 0`, and the same for `KobIfdPair` →
/// `KobCondPair`.
#[test]
fn entries_write_their_exit_block_with_bought_zero() {
    let d = Delivery { mode: 2, recv_threshold: 500_000_000, topup_tip: 12_345, bought: 0 };
    // KobIfdAsk
    let ia = match compiled(TemplateId::KobIfdAsk) {
        AnyState::KobIfdAsk(s) => s,
        _ => unreachable!(),
    };
    let today = ia.exit_for(7, None).unwrap().try_encode_layout(TemplateId::KobCondBid, StateLayout::Legacy).unwrap();
    assert_eq!(today.len(), 381);
    let ia2 = IfdAskState { exit_mode: d.mode, exit_recv_threshold: d.recv_threshold, exit_topup_tip: d.topup_tip, ..ia.clone() };
    let x = ia2.exit_for(7, None).unwrap();
    assert_eq!((x.mode, x.recv_threshold, x.topup_tip, x.bought), (d.mode, d.recv_threshold, d.topup_tip, 0));
    let span = x.try_encode_layout(TemplateId::KobCondBid, StateLayout::Delivery).unwrap();
    assert_eq!(span, [today.clone(), int_pushes(&[d.mode, d.recv_threshold, d.topup_tip, 0])].concat());
    assert_eq!(span.len(), 417);
    // booked exits carry the same block behind the repeat fields
    let booked = ia2.exit_for(7, Some(Booking { parent: [3; 32], until: 99 })).unwrap();
    assert_eq!(booked.try_encode_layout(TemplateId::KobCondBid, StateLayout::Delivery).unwrap()[381..], span[381..]);
    // ablation: an exit that ignored the entry's block would be mode 0
    assert_ne!(ia.exit().unwrap(), ia2.exit().unwrap());
    // KobIfdPair
    let ip = match compiled(TemplateId::KobIfdPair) {
        AnyState::KobIfdPair(s) => s,
        _ => unreachable!(),
    };
    let today = ip.exit_for(5, 6, None).unwrap().try_encode_layout(TemplateId::KobCondPair, StateLayout::Legacy).unwrap();
    assert_eq!(today.len(), 543);
    let ip2 = IfdPairState { exit_mode: 1, exit_recv_threshold: 0, exit_topup_tip: 0, ..ip };
    let x = ip2.exit_for(5, 6, None).unwrap();
    let span = x.try_encode_layout(TemplateId::KobCondPair, StateLayout::Delivery).unwrap();
    assert_eq!(span, [today, int_pushes(&[1, 0, 0, 0])].concat());
    assert_eq!(span.len(), 579);
}

/// The mutable windows: the legacy ones do not move, and in the delivery layout `bought` is the
/// last window of a delivering order, at the redeem offsets of the spec table.
#[test]
fn bought_is_the_last_mutable_window_of_the_delivery_layout() {
    let mut r = Rng(7);
    for (id, .., bought) in SPEC {
        let legacy = mutable_windows_in(id, StateLayout::Legacy);
        let delivery = mutable_windows_in(id, StateLayout::Delivery);
        match bought {
            Some((a, b)) => {
                assert_eq!(&delivery[..delivery.len() - 1], legacy, "{}", id.name());
                assert_eq!(delivery.last(), Some(&("bought", a, b)), "{}", id.name());
            }
            None => assert_eq!(delivery, legacy, "{}: an entry's block is immutable", id.name()),
        }
        assert_eq!(mutable_windows(id), mutable_windows_in(id, pinned_layout(id)));
        // every window is where its field sits in `prefix ‖ state` (1-byte prefix)
        let base = with_block(&compiled(id), any_block(&mut r));
        let rs = [vec![0u8], base.try_encode_layout(StateLayout::Delivery).unwrap()].concat();
        for (k, (field, a, b)) in delivery.iter().enumerate() {
            let v = 0x0a0b_0c0d_0e0f_0000 + k as i64;
            let mut j = serde_json::to_value(&base).unwrap();
            j["state"][*field] = serde_json::Value::String(v.to_string());
            let changed: AnyState = serde_json::from_value(j).unwrap();
            let rs2 = [vec![0u8], changed.try_encode_layout(StateLayout::Delivery).unwrap()].concat();
            assert_eq!(&rs2[*a..*b], &v.to_le_bytes(), "{} {field}", id.name());
            assert_eq!(&rs2[..*a], &rs[..*a], "{} {field}", id.name());
            assert_eq!(&rs2[*b..], &rs[*b..], "{} {field}", id.name());
        }
    }
    assert!(mutable_windows_in(TemplateId::KobBid, StateLayout::Legacy).is_empty());
    assert!(mutable_windows_in(TemplateId::KobBidKron, StateLayout::Delivery).is_empty());
}

// ---------------------------------------------------------------- delivery rules

fn d(mode: i64, recv_threshold: i64, topup_tip: i64, bought: i64) -> Delivery {
    Delivery { mode, recv_threshold, topup_tip, bought }
}

/// Every delivering kind with a valid KCC-20 delivery and a carrier of 1000 sompi.
fn delivering_states() -> Vec<AnyState> {
    let carrier = 1_000;
    let mut v = vec![];
    if let AnyState::KobBid(s) = compiled(TemplateId::KobBid) {
        v.push(AnyState::KobBid(BidState { delivery_carrier: carrier, ..s }));
    }
    if let AnyState::KobCondBid(s) = compiled(TemplateId::KobCondBid) {
        v.push(AnyState::KobCondBid(CondBidState { delivery_carrier: carrier, ..s }));
    }
    if let AnyState::KobPair(s) = compiled(TemplateId::KobPair) {
        v.push(AnyState::KobPair(PairState { delivery_carrier: carrier, s_family: 1, t_family: 1, ..s }));
    }
    if let AnyState::KobCondPair(s) = compiled(TemplateId::KobCondPair) {
        v.push(AnyState::KobCondPair(CondPairState { delivery_carrier: carrier, s_family: 1, t_family: 1, ..s }));
    }
    v
}

/// §2 *Placement* and §4 rules 1-2, one positive and one negative case per rule.
#[test]
fn delivery_rules_of_delivering_orders() {
    for s in delivering_states() {
        let n = s.template_id().name();
        let ok = |x: Delivery, placement: bool| with_block(&s, x).check_delivery(placement);
        // positive cases
        for (x, placement) in [
            (d(0, 0, 0, 0), true),
            (d(1, 0, 0, 0), true),
            (d(1, 0, 0, 5), false),
            (d(2, 1, 0, 0), true),
            (d(2, 500_000_000, 1_000, 0), true),
        ] {
            assert_eq!(ok(x, placement), Ok(()), "{n} {x:?}");
        }
        // negative cases, each failing exactly its rule
        for (x, placement, why) in [
            (d(3, 0, 0, 0), true, "mode must be 0, 1 or 2"),
            (d(-1, 0, 0, 0), true, "mode must be 0, 1 or 2"),
            (d(0, 1, 0, 0), true, "modes 0 and 1"),
            (d(1, 0, 1, 0), true, "modes 0 and 1"),
            (d(2, 0, 0, 0), true, "recvThreshold > 0"),
            (d(2, 1, 1_001, 0), true, "topupTip <= deliveryCarrier"),
            (d(2, 1, -1, 0), true, "topupTip <= deliveryCarrier"),
            (d(0, 0, 0, 5), false, "only mode 1 holds"),
            (d(2, 1, 0, 5), false, "only mode 1 holds"),
            (d(1, 0, 0, -1), false, "bought must be >= 0"),
            (d(1, 0, 0, 5), true, "a new order has bought = 0"),
        ] {
            let e = ok(x, placement).unwrap_err();
            assert!(e.contains(why), "{n} {x:?}: {e}");
        }
    }
}

/// A KRON delivery is mode 0: a pair order buying a KRON token, a KRON twin, and the exits of an
/// entry that deliver a KRON token; a repeating entry's exits are mode 0.
#[test]
fn kron_deliveries_and_repeating_entries_are_mode_zero() {
    for s in delivering_states().into_iter().filter(|s| s.is_pair()) {
        let kron_t = match &s {
            AnyState::KobPair(x) => AnyState::KobPair(PairState { t_family: 2, ..x.clone() }),
            AnyState::KobCondPair(x) => AnyState::KobCondPair(CondPairState { t_family: 2, ..x.clone() }),
            _ => unreachable!(),
        };
        assert_eq!(with_block(&kron_t, d(0, 0, 0, 0)).check_delivery(true), Ok(()));
        assert!(with_block(&kron_t, d(1, 0, 0, 0)).check_delivery(true).unwrap_err().contains("KRON delivery is mode 0"));
        // the KCC-20 twin of the same order may accumulate (the rule is the KRON one)
        assert_eq!(with_block(&s, d(1, 0, 0, 0)).check_delivery(true), Ok(()));
    }
    let kron = with_block(&compiled(TemplateId::KobBidKron), d(1, 0, 0, 0));
    assert!(kron.check_delivery(true).unwrap_err().contains("KRON delivery is mode 0"));

    // KobIfdAsk: exits of the entry's own token; KRON entries cannot hold a block at all
    if let AnyState::KobIfdAsk(ia) = compiled(TemplateId::KobIfdAsk) {
        let carrier = ia.exit().unwrap().delivery_carrier;
        let once = AnyState::KobIfdAsk(IfdAskState { rpt_amount: 0, ..ia.clone() });
        assert_eq!(with_block(&once, d(1, 0, 0, 0)).check_delivery(true), Ok(()));
        assert_eq!(with_block(&once, d(2, 9, carrier, 0)).check_delivery(true), Ok(()));
        assert!(with_block(&once, d(2, 9, carrier + 1, 0)).check_delivery(true).unwrap_err().contains("topupTip"));
        let repeating = AnyState::KobIfdAsk(IfdAskState { rpt_amount: 10, ..ia });
        assert_eq!(with_block(&repeating, d(0, 0, 0, 0)).check_delivery(true), Ok(()));
        assert!(with_block(&repeating, d(1, 0, 0, 0)).check_delivery(true).unwrap_err().contains("repeating entry"));
    }
    // KobIfdPair: the exits deliver B (buy-first) or A (sell-first)
    if let AnyState::KobIfdPair(ip) = compiled(TemplateId::KobIfdPair) {
        for side in [SIDE_BID, SIDE_ASK] {
            let base = IfdPairState { side, a_family: 1, b_family: 1, rpt_amount: 0, ..ip.clone() };
            let delivered_kron = if side == SIDE_BID {
                IfdPairState { b_family: 2, ..base.clone() }
            } else {
                IfdPairState { a_family: 2, ..base.clone() }
            };
            let other_kron = if side == SIDE_BID {
                IfdPairState { a_family: 2, ..base.clone() }
            } else {
                IfdPairState { b_family: 2, ..base.clone() }
            };
            assert_eq!(with_block(&AnyState::KobIfdPair(base.clone()), d(1, 0, 0, 0)).check_delivery(true), Ok(()));
            assert_eq!(with_block(&AnyState::KobIfdPair(other_kron), d(1, 0, 0, 0)).check_delivery(true), Ok(()), "side {side}");
            let e = with_block(&AnyState::KobIfdPair(delivered_kron), d(1, 0, 0, 0)).check_delivery(true).unwrap_err();
            assert!(e.contains("KRON delivery is mode 0"), "side {side}: {e}");
            let rep = AnyState::KobIfdPair(IfdPairState { rpt_amount: 3, ..base });
            assert!(with_block(&rep, d(1, 0, 0, 0)).check_delivery(true).unwrap_err().contains("repeating entry"));
        }
    }
}

/// The accessors: a mode-1 order's bought-custody, `with_bought`, and the amend comparison.
#[test]
fn bought_custody_and_same_delivery() {
    for s in delivering_states() {
        let one = with_block(&s, d(1, 0, 0, 0));
        assert_eq!(one.bought_custody(), None);
        let held = one.with_bought(77).unwrap();
        assert_eq!(held.bought_custody(), Some(77));
        assert_eq!(held.delivery().unwrap().bought, 77);
        assert_eq!(with_block(&s, d(0, 0, 0, 0)).with_bought(77).unwrap().bought_custody(), None, "mode 0 holds none");
        assert!(held.same_delivery(&held.clone()));
        assert!(!held.same_delivery(&one));
        assert!(!one.same_delivery(&with_block(&s, d(2, 1, 0, 0))));
    }
    assert_eq!(compiled(TemplateId::KobAsk).with_bought(1), None);
    assert_eq!(compiled(TemplateId::KobAsk).delivery(), None);
}

// ---------------------------------------------------------------- golden vectors

const VECTORS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/state/delivery_vectors.json");

/// The vector states: each delivering kind with a non-zero block.
fn vector_states() -> Vec<AnyState> {
    let mut v = vec![];
    for s in delivering_states() {
        let x = match s.template_id() {
            TemplateId::KobBid => d(1, 0, 0, 123_456_789),
            TemplateId::KobCondBid => d(2, 500_000_000, 777, 0),
            TemplateId::KobPair => d(1, 0, 0, 5_000),
            _ => d(2, 1_234, 1_000, 0),
        };
        v.push(with_block(&s, x));
    }
    if let AnyState::KobIfdAsk(ia) = compiled(TemplateId::KobIfdAsk) {
        let x = ia.exit().unwrap();
        let tip = x.delivery_carrier.clamp(0, 300);
        // a valid entry: its committed exit at its own scale
        let ia = IfdAskState { scale: x.scale, ..ia };
        v.push(with_block(&AnyState::KobIfdAsk(IfdAskState { rpt_amount: 0, ..ia }), d(2, 250_000_000, tip, 0)));
    }
    if let AnyState::KobIfdPair(ip) = compiled(TemplateId::KobIfdPair) {
        v.push(with_block(&AnyState::KobIfdPair(IfdPairState { rpt_amount: 0, ..ip }), d(1, 0, 0, 0)));
    }
    v
}

fn vector_json() -> serde_json::Value {
    let rows: Vec<serde_json::Value> = vector_states()
        .iter()
        .map(|s| {
            let id = s.template_id();
            let b = delivery_block(id).unwrap();
            let span = s.try_encode_layout(StateLayout::Delivery).unwrap();
            let legacy = with_block(s, Delivery::NEW).try_encode_layout(StateLayout::Legacy).unwrap();
            serde_json::json!({
                "kind": id.name(),
                "legacyLen": b.legacy_len,
                "deliveryLen": b.len(),
                "order": s,
                "block": crate::json::to_hex(&span[b.legacy_len..]),
                "delivery": crate::json::to_hex(&span),
                "legacy": crate::json::to_hex(&legacy),
            })
        })
        .collect();
    serde_json::json!({
        "about": "bid-delivery-spec §2 golden vectors: each delivering kind's state in the delivery layout (order, delivery) and the same terms in the legacy layout (legacy, every block field zero). block = delivery[legacyLen..]: one 0x08 push of 8 LE bytes per field (mode, recvThreshold, topupTip, bought; an entry: exitMode, exitRecvThreshold, exitTopupTip). Regenerate: KOB_REGEN=1 cargo test -p kob-protocol --lib delivery_golden_vectors",
        "vectors": rows,
    })
}

/// The golden vectors pin both layouts of every delivering kind, independent of the artifacts: the
/// states are spelled out in full, the spans must encode and decode exactly.
#[test]
fn delivery_golden_vectors() {
    if std::env::var("KOB_REGEN").is_ok_and(|v| v == "1") {
        std::fs::write(VECTORS, serde_json::to_string_pretty(&vector_json()).unwrap() + "\n").unwrap();
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(VECTORS).unwrap()).unwrap();
    let rows = v["vectors"].as_array().unwrap();
    assert_eq!(rows.len(), SPEC.len());
    for (row, (id, legacy_len, _, len, _)) in rows.iter().zip(SPEC) {
        let hex = |k: &str| crate::json::from_hex(row[k].as_str().unwrap()).unwrap();
        let order: AnyState = serde_json::from_value(row["order"].clone()).unwrap();
        assert_eq!(order.template_id(), id);
        assert_eq!((row["legacyLen"].as_u64(), row["deliveryLen"].as_u64()), (Some(legacy_len as u64), Some(len as u64)));
        let (span, legacy, block) = (hex("delivery"), hex("legacy"), hex("block"));
        assert_eq!(order.try_encode_layout(StateLayout::Delivery).unwrap(), span, "{}", id.name());
        assert_eq!(typed_layout(id, &span).unwrap(), (order.clone(), StateLayout::Delivery), "{}", id.name());
        assert_eq!(span.len(), len);
        assert_eq!(&span[..legacy_len], &legacy[..], "{}", id.name());
        assert_eq!(&span[legacy_len..], &block[..], "{}", id.name());
        let b = order.block_fields().unwrap();
        assert_eq!(block, int_pushes(&block_values(id, b)), "{}", id.name());
        assert_ne!(b, Delivery::NEW, "{}: a vector carries a non-zero block", id.name());
        let zero = with_block(&order, Delivery::NEW);
        assert_eq!(typed_layout(id, &legacy).unwrap(), (zero.clone(), StateLayout::Legacy), "{}", id.name());
        assert_eq!(zero.try_encode_layout(StateLayout::Legacy).unwrap(), legacy);
        assert!(order.check_delivery(false).is_ok(), "{}", id.name());
        order.validate_layout(StateLayout::Delivery).unwrap_or_else(|e| panic!("{}: {e}", id.name()));
    }
    assert_eq!(v, vector_json(), "the vectors are stale: KOB_REGEN=1 rewrites them");
}

/// A mode-1 bid that holds its bought-custody (`bought > 0`) reserves no carrier: its fills grow the custody and move no
/// carrier (`KobBid.sil` `resv`), so its buying power and whether it can continue count the escrow without one. A mode-1
/// bid before its first fill (bought 0) and modes 0 / 2 keep one carrier per further fill.
#[test]
fn a_bid_holding_its_bought_custody_reserves_no_carrier() {
    let AnyState::KobBid(s) = compiled(TemplateId::KobBid) else { unreachable!() };
    // used(n) = 10 n sompi
    let base = BidState {
        scale: 1,
        price: 10,
        tip: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1,
        min_fill: 1,
        max_fill: 0,
        reserve: 0,
        delivery_carrier: 1_000,
        recv_threshold: 0,
        topup_tip: 0,
        ..s
    };
    for (mode, bought, power, cont) in [(0, 0, 100, false), (1, 0, 100, false), (1, 5, 200, true), (2, 0, 100, false)] {
        let (recv_threshold, topup_tip) = if mode == 2 { (1, 0) } else { (0, 0) };
        let b = BidState { mode, bought, recv_threshold, topup_tip, ..base.clone() };
        assert_eq!(b.carrier_reserve(), if cont { 0 } else { 1_000 }, "mode {mode} bought {bought}");
        assert_eq!(b.buying_power(2_000), power, "mode {mode} bought {bought}: buying power of 2,000 sompi");
        // 1,005 sompi left after a fill: one more minimum fill (10 sompi) only without a carrier
        assert_eq!(b.can_continue(1_005), cont, "mode {mode} bought {bought}: can continue");
        // minFill 2 (20 sompi): a fill of 1 from 1,020 leaves 1,010, which funds another minimum fill only without a
        // carrier, so the short fill is allowed exactly when the bid cannot continue
        assert_eq!(BidState { min_fill: 2, ..b.clone() }.fill_ok(1, 1_020), !cont, "mode {mode} bought {bought}: minimum fill");
    }
    // with a reserve the carrier-free budget still keeps it
    let b = BidState { mode: 1, bought: 5, reserve: 300, ..base };
    assert_eq!(b.buying_power(2_000), 170);
    assert!(!b.can_continue(305));
    assert!(b.can_continue(310));
}
