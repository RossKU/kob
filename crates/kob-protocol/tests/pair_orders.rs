//! Tests the pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`) through the builders, with every
//! transaction validated in the script engine.
//!
//! Each kind has one template for both sides and both families of A and B. Covered:
//!
//! * creation (both custodies of a sell-first entry) and cancel (strays of both tokens);
//! * refunds and kills;
//! * fills routed through the KAS books, and netting of opposite pair orders (1 x 1, 2 x 2);
//! * conditional fills armed in the fill by evidence (two KAS-book fills, or a resting pair order);
//! * updates (arm, trail) in both evidence modes, and an armed stop's auction;
//! * if-done fills, and bookings and merges of both sides;
//! * the builders' refusals and the maker-value checks (a maker never gets less than its terms).
//!
//! The program pairs cover every family mix. Every transaction runs every order covenant and token
//! program in the rusty-kaspa v2.1.0 engine and pays the node's relay floor.

mod common;

use common::exact::run_measured as run_any;
use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{Action, Batch, Leg, RefundOrder};
use kob_protocol::state::*;
use kob_protocol::tx::{spk_to_string, SignedTx, TokenUtxo};

/// Program pairs (A, B) of every family mix, the large third-party program included.
const PAIRS: [(TemplateId, TemplateId); 6] = [
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref8x8),
    (TemplateId::KronToken2433, TemplateId::KronToken2732),
    (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref4x5, TemplateId::Kcc20KaspaCom025),
];

fn run(label: &str, a: &Action) -> SignedTx {
    let (_, signed) = run_any(label, a);
    println!("{label}: {} bytes mass size, fee {} sompi", signed.fee.mass.size, signed.fee.fee);
    signed
}

#[test]
fn every_pair_shape_validates_on_every_family_mix() {
    for (pa, pb) in PAIRS {
        for (name, a) in pair_scenarios(pa, pb) {
            run(&format!("{name} @{}", pair_name(pa, pb)), &a);
        }
    }
}

/// The token state at output `at`: `amount` of `token` (program `p`) owned by key `owner`.
fn token_at(signed: &SignedTx, at: usize, p: TemplateId, token: [u8; 32], owner: [u8; 32], amount: i64) -> bool {
    let o = &signed.tx.outputs[at];
    let st = TokenState::user(p.family(), amount, owner, ext_for(p));
    o.covenant.as_ref().map(|c| c.covenant_id) == Some(token)
        && spk_to_string(&st.spk_with(kob_protocol::artifacts::token_template(p))) == o.script_public_key
}

/// The maker never gets less than its terms: a pair ask's B delivery is at least the ceil of its price, a pair bid receives
/// exactly its A and pays exactly the floor (its escrow rest holds the rest), whatever the route or the netting.
#[test]
fn makers_get_at_least_their_terms() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433);
    // a pair ask through the KAS books: 4 whole A at 1.00 B -> at least 4000 B (the ask of B sells exactly 4000)
    let s = run("ask", &Action::Batch(route_ask(pair(MAKER_A, true, pa, pb, RATE), pa, pb, 4)));
    assert!(token_at(&s, 0, pb, TOKEN_B, pk(MAKER_A), 4 * RATE), "the ask's delivery");
    // an odd price: 3 whole A at 0.333 B -> ceil(3000 * 333 / 1000) = 999; the route's ask of B sells 1000: the surplus
    // unit rides on the maker's delivery
    let s = run("ask.odd", &Action::Batch(route_ask(pair(MAKER_A, true, pa, pb, 333), pa, pb, 1)));
    assert!(token_at(&s, 0, pb, TOKEN_B, pk(MAKER_A), WHOLE), "the ask's delivery takes the surplus");
    // a pair bid: exactly n of A delivered
    let s = run("bid", &Action::Batch(route_bid(pair(MAKER_A, false, pa, pb, RATE), pa, pb, 4)));
    assert!(token_at(&s, 0, pa, TOKEN_COV, pk(MAKER_A), 4 * WHOLE), "the bid's delivery");
    // netting 1 x 1: the bid pays 1.01 B per A, the ask gets all of it
    let s = run("net", &Action::Batch(netting(pa, pb, 1, 1, false)));
    assert!(token_at(&s, 0, pb, TOKEN_B, pk(MAKER_A), WHOLE + 10), "the ask gets the bid's whole payment");
    assert!(token_at(&s, 1, pa, TOKEN_COV, pk(20), WHOLE), "the bid gets its A");
}

/// Builder refusals of what the covenants refuse (and of what the rules of the order kinds forbid).
#[test]
fn builders_refuse_what_the_covenants_refuse() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let build = |b: Batch| common::exact::build_measured(&Action::Batch(b));
    let ask = pair(MAKER_A, true, pa, pb, RATE);
    // below the minimum fill (not the whole amount)
    let small = route_ask(PairState { min_fill: 2 * WHOLE, ..ask.clone() }, pa, pb, 1);
    assert!(build(small).unwrap_err().to_string().contains("minFill"));
    // FOK partial
    assert!(build(route_ask(PairState { tif: TIF_FOK, ..ask.clone() }, pa, pb, 4)).is_err());
    // not active yet
    assert!(build(route_ask(PairState { active_from: NOW as i64 + 1, ..ask.clone() }, pa, pb, 4)).is_err());
    // a custody that is not exactly the order's
    let mut b = route_ask(ask.clone(), pa, pb, 4);
    if let Leg::Pair { custody, .. } = &mut b.legs[0] {
        custody.state = custody.state.with_amount(custody.state.amount() - 1);
    }
    assert!(build(b).is_err());
    // a look-alike of the custody owned by another covenant id, or carrying a borrow setting (R8F3c-1)
    let mut b = route_ask(ask.clone(), pa, pb, 4);
    if let Leg::Pair { custody, .. } = &mut b.legs[0] {
        if let TokenState::Kcc20(k) = &mut custody.state {
            k.owner = [0x5c; 32];
        }
    }
    assert!(build(b).unwrap_err().to_string().contains("not owned by its order"));
    let mut b = route_ask(ask.clone(), pa, pb, 4);
    if let Leg::Pair { custody, .. } = &mut b.legs[0] {
        if let TokenState::Kcc20(k) = &mut custody.state {
            k.borrow_scheme = 1;
        }
    }
    assert!(build(b).is_err());
    // the delivery the route cannot fund (the ask of B sells too little)
    let mut b = route_ask(ask.clone(), pa, pb, 4);
    if let Leg::Ask { amount, .. } = &mut b.legs[2] {
        *amount = 3 * WHOLE;
    }
    assert!(build(b).unwrap_err().to_string().contains("delivers"));
    // a bid whose escrow cannot pay
    let bid = pair(MAKER_A, false, pa, pb, RATE);
    assert!(build(route_bid(PairState { custody: 3_999, tif: TIF_IOC, ..bid.clone() }, pa, pb, 4)).is_err());
    // an unarmed stop leg without evidence, and with evidence of the wrong side (a pair BID arms no sell stop)
    let ca = cond_pair_ask(MAKER_A, pa, pb);
    let b = route(vec![
        cond_pair_leg(ca.clone(), pa, pb, X_ID, 70, 4, 1),
        kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
        kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 4),
    ]);
    assert!(build(b).unwrap_err().to_string().contains("evidence"));
    let wrong = with_evidence(
        route(vec![
            cond_pair_leg(ca.clone(), pa, pb, X_ID, 70, 4, 1),
            pair_leg(pair(MAKER_C, false, pa, pb, RATE - 10), pa, pb, cov(0x93), 72, 1),
            pair_leg(pair(MAKER_B, true, pa, pb, RATE), pa, pb, cov(0x94), 74, 5),
        ]),
        1,
        None,
    );
    assert!(build(wrong).is_err());
    // evidence that does not reach the stop (a pair ASK at 1.01 > the 1.00 sell stop)
    let high = with_evidence(
        route(vec![
            cond_pair_leg(ca, pa, pb, X_ID, 70, 4, 1),
            pair_leg(pair(MAKER_C, true, pa, pb, RATE + 10), pa, pb, cov(0x93), 72, 1),
            pair_leg(pair(MAKER_B, false, pa, pb, RATE + 20), pa, pb, cov(0x94), 74, 5),
        ]),
        1,
        None,
    );
    assert!(build(high).unwrap_err().to_string().contains("arms only"));
    // a KRON custody above 10^9 base units (both pinned KRON programs refuse such a UTXO) is refused by every fill builder
    let k = TemplateId::KronToken2433;
    let big = AskState { token_cov_id: TOKEN_B, amount_left: 1_500_000_000, ..common::ask(MAKER_C, P250, k) };
    let mut b = route(vec![kbid_units(TOKEN_B, k, MAKER_B, P260, cov(0x91), 72, WHOLE)]);
    b.legs.insert(
        0,
        Leg::Ask {
            order: order(70, CARRIER, cov(0x92), 1_000, big),
            custody: tutxo(k, TOKEN_B, 71, 1_500_000_000, cov(0x92), true),
            amount: WHOLE,
            t: None,
        },
    );
    assert!(build(b).unwrap_err().to_string().contains("KRON custody"));
    // creation rules
    assert!(kob_protocol::build::check_new_order(&AnyState::KobPair(PairState { custody: 9 * WHOLE, ..ask.clone() })).is_err());
    assert!(kob_protocol::build::check_new_order(&AnyState::KobPair(PairState { t_cov_id: TOKEN_COV, ..ask.clone() })).is_err());
    assert!(kob_protocol::build::check_new_order(&AnyState::KobPair(PairState { custody: 9_999, ..bid })).is_err());
    let ia = ifd_pair(MAKER_A, false, pa, pb);
    assert!(
        kob_protocol::build::check_new_order(&AnyState::KobIfdPair(IfdPairState { custody: ia.custody - 1, ..ia.clone() })).is_err()
    );
    assert!(
        kob_protocol::build::check_new_order(&AnyState::KobIfdPair(IfdPairState { prefund: 0, ..ia })).is_err(),
        "prefund covers the buy-back"
    );
}

/// The layout keeps every positional token output (a rest or a return at its custody's input index)
/// right after the order inputs, whichever token it is of: a transaction whose SECOND token has an
/// IOC return while its first token has several custodies without one leaves no output slot
/// reserved but never filled.
#[test]
fn positional_outputs_of_any_token_leave_no_gap() {
    let p = TemplateId::Kcc20Ref8x8;
    // token A: three asks selling out (custodies without an output at their index); token B: an IOC ask keeping a return
    let mut legs = vec![];
    for k in 0..3u8 {
        legs.push(kask_leg(TOKEN_COV, p, 20 + k, P250, cov(0xa0 + k), 100 + 2 * k, 1, 1));
    }
    let ioc = AskState { tif: TIF_IOC, token_cov_id: TOKEN_B, amount_left: 5 * WHOLE, ..ask(MAKER_A, P250, p) };
    legs.push(Leg::Ask {
        order: order(120, CARRIER, cov(0xb0), 1_000, ioc),
        custody: tutxo(p, TOKEN_B, 121, 5 * WHOLE, cov(0xb0), true),
        amount: WHOLE,
        t: None,
    });
    legs.push(kbid_leg(TOKEN_COV, p, 30, P260, cov(0xb1), 130, 3, 3));
    legs.push(kbid_leg(TOKEN_B, p, 31, P260, cov(0xb2), 132, 1, 1));
    run("ioc return of the second token", &Action::Batch(route(legs)));
}

/// Diagnostic: every scenario on one program pair, reporting each outcome without stopping (`--ignored`).
#[test]
#[ignore]
fn diag_pair_shapes() {
    use kob_protocol::tx::{finalize, sign_locally, FinalizeOptions};
    let pairs: Vec<(TemplateId, TemplateId)> = match std::env::var("KOB_DIAG_ALL") {
        Ok(_) => {
            let ps = common::pair_programs();
            ps.iter().flat_map(|a| ps.iter().map(move |b| (*a, *b))).collect()
        }
        Err(_) => vec![(TemplateId::Kcc20Ref, TemplateId::Kcc20Ref), (TemplateId::KronToken2433, TemplateId::Kcc20P2)],
    };
    for (pa, pb) in pairs {
        for (name, a) in pair_scenarios(pa, pb).into_iter().chain(pair_branch_shapes(pa, pb)) {
            let name = format!("{name}@{}", pair_name(pa, pb));
            let r = common::exact::build_measured(&a).and_then(|built| {
                let signed = finalize(&built, &sign_locally(&built, &keys()).unwrap(), FinalizeOptions::default())?;
                kob_protocol::verify::validate_signed(&signed).map(|_| built.roles.clone())
            });
            match r {
                Ok(_) => println!("OK   {name}"),
                Err(e) => println!("ERR  {name}: {e}"),
            }
        }
    }
}

/// A pair BID's escrow is the floor of its whole amount at its highest price plus ONE unit, whatever its minimum fill (no
/// slack per fill: each fill pays exactly its floor and floors are subadditive): a GTC bid of minFill one unit filled one
/// small slice at a time pays every slice and keeps a positive rest to the end. The same for a conditional pair BID at its
/// worst leg.
#[test]
fn a_pair_bid_escrow_funds_every_split_with_one_unit_of_slack() {
    let (pa, pb) = (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref);
    // an awkward price (a remainder on every slice) and minFill one base unit
    let p = RATE + 237;
    let mut y = PairState { min_fill: 1, price: p, ..pair(MAKER_A, false, pa, pb, p) };
    y.custody = y.bid_escrow(y.amount_left, 0).unwrap();
    assert_eq!(y.custody, (y.amount_left * p) / SCALE + 1, "floor of the whole amount plus one unit");
    assert!(y.custody < 2 * y.amount_left * p / SCALE, "no per-fill slack (minFill 1 used to double the escrow)");
    for n in std::iter::repeat_n(7, 1_000).chain([3, 1]) {
        let n = n.min(y.amount_left);
        let f = y.fill(n, p, PV as i64).unwrap_or_else(|e| panic!("a slice of {n} with {} left: {e}", y.amount_left));
        assert!(!f.rest || f.out_amount > 0);
        y = PairState { amount_left: y.amount_left - n, custody: f.out_amount, ..y };
        if y.amount_left == 0 {
            break;
        }
    }
    // the rest of the whole amount in one fill
    if y.amount_left > 0 {
        assert!(y.fill(y.amount_left, p, PV as i64).is_ok());
    }
    assert!(kob_protocol::build::check_new_order(&AnyState::KobPair(PairState {
        custody: PairState { min_fill: 1, price: p, ..pair(MAKER_A, false, pa, pb, p) }.bid_escrow(10 * WHOLE, 0).unwrap(),
        min_fill: 1,
        price: p,
        ..pair(MAKER_A, false, pa, pb, p)
    }))
    .is_ok());
    // the conditional pair BID: the floor at its worst leg plus one
    let c = CondPairState { min_fill: 1, ..cond_pair_bid(MAKER_A, pa, pb) };
    let worst = c.worst();
    assert_eq!(c.bid_escrow(c.max_fills()).unwrap(), c.amount_left * worst / SCALE + 1);
}

/// A new pair order that can rest after a fill must fund a partial fill AND the fill of its rest (each continuation pays
/// its own delivery and keeps a positive value): below that value its creation is refused. An IOC / FOK order, or one
/// whose minFill takes everything, funds one delivery; a TWAP / DCA order one per maxFill slice.
#[test]
fn pair_orders_are_funded_for_a_partial_fill_and_its_rest() {
    use kob_protocol::build::min_order_value;
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433);
    let x = PairState { tip: 10_000, ..pair(MAKER_A, true, pa, pb, RATE) };
    let tip = x.tip_kas(x.amount_left).unwrap();
    assert_eq!(min_order_value(&AnyState::KobPair(x.clone())), 2 * PDC + tip);
    assert_eq!(min_order_value(&AnyState::KobPair(PairState { tif: TIF_IOC, ..x.clone() })), PDC + tip);
    assert_eq!(min_order_value(&AnyState::KobPair(PairState { tif: TIF_FOK, ..x.clone() })), PDC + tip);
    assert_eq!(min_order_value(&AnyState::KobPair(PairState { min_fill: x.amount_left, ..x.clone() })), PDC + tip);
    let twap = PairState { interval: 100, max_fill: 2 * WHOLE + 1, ..x.clone() };
    assert_eq!(min_order_value(&AnyState::KobPair(twap)), 5 * PDC + tip, "ceil(10 / 2.001) = 5 slices");
    let c = cond_pair_ask(MAKER_A, pa, pb);
    assert_eq!(min_order_value(&AnyState::KobCondPair(c.clone())), 2 * PDC);
    assert_eq!(min_order_value(&AnyState::KobCondPair(CondPairState { min_fill: c.amount_left, ..c })), PDC);
    // the creation of a GTC ask funded for one delivery only is refused (its first partial fill would leave nothing)
    let (_, create) = pair_scenarios(pa, pb).into_iter().find(|(n, _)| n == "pair.create.ask").expect("the ask creation");
    let Action::CreateOrder(mut r) = create else { panic!("a creation") };
    let AnyState::KobPair(s) = &r.order else { panic!("a pair order") };
    r.value = s.kas_value(1).unwrap() as u64;
    let e = kob_protocol::build::build(&Action::CreateOrder(r.clone())).unwrap_err().to_string();
    assert!(e.contains("needs to be fillable"), "{e}");
    r.value = s.kas_value(2).unwrap() as u64;
    assert!(kob_protocol::build::build(&Action::CreateOrder(r)).is_ok());
}

/// An if-done entry's exitCarrier funds what its exit needs by the exit's own rules: the keeper's arming tip, then its
/// own deliveries (a partial fill and its rest) and the tip of the entry's whole amount, or its refund tip; one sompi less
/// is refused at creation.
#[test]
fn an_if_done_exit_carrier_funds_the_exit() {
    let (pa, pb) = (TemplateId::Kcc20Ref, TemplateId::KronToken2433);
    for buy in [true, false] {
        let i = ifd_pair(MAKER_A, buy, pa, pb);
        let e = i.exit().unwrap();
        let need = i.exit_carrier_needed().unwrap();
        let tip = CondPairState { amount_left: i.amount_left, ..e.clone() }.tip_kas(i.amount_left).unwrap();
        assert_eq!(need, e.keeper_tip + 2 * e.delivery_carrier + tip);
        let ok = |c: i64| kob_protocol::build::check_new_order(&AnyState::KobIfdPair(IfdPairState { exit_carrier: c, ..i.clone() }));
        assert!(ok(need).is_ok(), "{:?}", ok(need));
        let err = ok(need - 1).unwrap_err().to_string();
        assert!(err.contains("exitCarrier"), "{err}");
        // a single-fill exit (minFill takes the whole amount) funds one delivery
        let mut x1 = e.clone();
        x1.min_fill = i.amount_left;
        let i1 = IfdPairState { exit_state: IfdPairState::commit_exit(&x1), ..i.clone() };
        assert_eq!(i1.exit_carrier_needed().unwrap(), e.keeper_tip + e.delivery_carrier + tip);
    }
}

// ---------------------------------------------------------------- delivery modes (docs/design/bid-delivery-spec.md §4)

/// A mode-2 order's `recvThreshold` (below the one-whole minimum fill) and `topupTip`.
const RECV_THR: i64 = 5;
const TOPUP_TIP: i64 = 1_000_000;

fn pair_mode(s: PairState, mode: i64) -> PairState {
    let (recv_threshold, topup_tip) = if mode == MODE_TOPUP { (RECV_THR, TOPUP_TIP) } else { (0, 0) };
    PairState { mode, recv_threshold, topup_tip, ..s }
}

fn cond_mode(s: CondPairState, mode: i64) -> CondPairState {
    let (recv_threshold, topup_tip) = if mode == MODE_TOPUP { (RECV_THR, TOPUP_TIP) } else { (0, 0) };
    CondPairState { mode, recv_threshold, topup_tip, ..s }
}

/// A receive UTXO state (bid-delivery-spec §1): the key's holding with borrow scheme 0x01 and the guard threshold `thr`
/// little-endian, then 24 zero bytes.
fn receive_state(amount: i64, owner: [u8; 32], thr: i64, ext: [u8; 32]) -> TokenState {
    let mut g = [0u8; 32];
    g[..8].copy_from_slice(&thr.to_le_bytes());
    TokenState::user(kob_protocol::Family::Kcc20, amount, owner, ext).with_borrow((1, g))
}

fn spk_of(s: &kaspa_consensus_core::tx::ScriptPublicKey) -> String {
    spk_to_string(s)
}

/// Output `at` holds token state `st` of `token` (program `p`).
fn holds(signed: &SignedTx, at: usize, p: TemplateId, token: [u8; 32], st: &TokenState) -> bool {
    let o = &signed.tx.outputs[at];
    o.covenant.as_ref().map(|c| c.covenant_id) == Some(token)
        && spk_of(&st.spk_with(kob_protocol::artifacts::token_template(p))) == o.script_public_key
}

/// The index of the output with script public key `spk`, if any.
fn output_with(signed: &SignedTx, spk: &kaspa_consensus_core::tx::ScriptPublicKey) -> Option<usize> {
    let s = spk_of(spk);
    signed.tx.outputs.iter().position(|o| o.script_public_key == s)
}

/// The T a pair leg buys: (covenant id, program).
fn t_of(ask: bool, pa: TemplateId, pb: TemplateId) -> ([u8; 32], TemplateId) {
    if ask {
        (TOKEN_B, pb)
    } else {
        (TOKEN_COV, pa)
    }
}

/// Program pairs with a KCC-20 T on at least one side.
const MODE_PAIRS: [(TemplateId, TemplateId); 3] = [
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref8x8),
];

/// Modes 1 (no bought-custody yet) and 2 (form 1, the default without a receive UTXO) of a `KobPair`, both sides, a
/// partial fill and the fill that ends the order, through the KAS books. Mode 1: output i is the order's bought-custody of
/// n (owned by the order's covenant id) and the continuation records bought = n; at the order's end the maker's plain
/// UTXO. Mode 2: a new receive UTXO of n with the order's threshold, the continuation unchanged. Each runs in the engine.
#[test]
fn pair_orders_deliver_in_modes_one_and_two() {
    for (pa, pb) in MODE_PAIRS {
        for ask in [true, false] {
            let (tok, tp) = t_of(ask, pa, pb);
            if tp.family() != kob_protocol::Family::Kcc20 {
                continue;
            }
            let ext = ext_for(tp);
            let cov = if ask { X_ID } else { Y_ID };
            let route_of = |x: PairState, n: i64| if ask { route_ask(x, pa, pb, n) } else { route_bid(x, pa, pb, n) };
            for mode in [MODE_ACCUMULATE, MODE_TOPUP] {
                let name = format!("{} mode {mode} @{}", if ask { "ask" } else { "bid" }, pair_name(pa, pb));
                // a partial fill of 4 whole A out of 10
                let x = pair_mode(pair(MAKER_A, ask, pa, pb, RATE), mode);
                let n = 4 * WHOLE;
                let t = x.t_out_min(n, x.price).unwrap();
                let s = run(&format!("{name} partial"), &Action::Batch(route_of(x.clone(), 4)));
                let f = x.fill(n, x.price, pv() as i64).unwrap();
                let next = PairState { amount_left: x.amount_left - n, custody: f.out_amount, ..x.clone() };
                if mode == MODE_ACCUMULATE {
                    assert!(holds(&s, 0, tp, tok, &TokenState::custody(tp.family(), t, cov, ext)), "{name}: the bought-custody of n");
                    assert!(output_with(&s, &PairState { bought: t, ..next.clone() }.spk()).is_some(), "{name}: bought = n");
                } else {
                    let r = receive_state(t, pk(MAKER_A), RECV_THR, ext);
                    assert!(holds(&s, 0, tp, tok, &r), "{name}: a new receive UTXO of n");
                    assert!(output_with(&s, &next.spk()).is_some(), "{name}: the continuation keeps bought = 0");
                }
                // the fill that ends the order
                let s = run(&format!("{name} end"), &Action::Batch(route_of(with_left(x.clone(), 4), 4)));
                let want = if mode == MODE_ACCUMULATE {
                    TokenState::user(tp, t, pk(MAKER_A), ext)
                } else {
                    receive_state(t, pk(MAKER_A), RECV_THR, ext)
                };
                assert!(holds(&s, 0, tp, tok, &want), "{name}: the ending delivery");
            }
        }
    }
}

/// The same for `KobCondPair`: a take-profit sell and a limit buy, partial, in modes 1 and 2.
#[test]
fn cond_pair_orders_deliver_in_modes_one_and_two() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    for ask in [true, false] {
        let (tok, tp) = t_of(ask, pa, pb);
        let ext = ext_for(tp);
        for mode in [MODE_ACCUMULATE, MODE_TOPUP] {
            let name = format!("cond {} mode {mode}", if ask { "ask" } else { "bid" });
            let c = cond_mode(if ask { cond_pair_ask(MAKER_A, pa, pb) } else { cond_pair_bid(MAKER_A, pa, pb) }, mode);
            let n = 4 * WHOLE;
            let t = c.t_out_min(n, c.tp_price).unwrap();
            let legs = if ask {
                vec![
                    cond_pair_leg(c.clone(), pa, pb, X_ID, 70, 4, 0),
                    kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
                    kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 5),
                ]
            } else {
                vec![
                    cond_pair_leg(c.clone(), pa, pb, X_ID, 70, 4, 0),
                    kbid_leg(TOKEN_B, pb, MAKER_B, P260, cov(0x91), 72, 10, 3),
                    kask_leg(TOKEN_COV, pa, MAKER_C, P250, cov(0x92), 74, 10, 4),
                ]
            };
            let s = run(&name, &Action::Batch(route(legs)));
            let want = if mode == MODE_ACCUMULATE {
                TokenState::custody(tp.family(), t, X_ID, ext)
            } else {
                // the ask's delivery takes the batch's surplus of B (the ask of B sells 5 whole B); a bought-custody that
                // stays with the order takes none
                receive_state(if ask { 5 * WHOLE } else { t }, pk(MAKER_A), RECV_THR, ext)
            };
            assert!(holds(&s, 0, tp, tok, &want), "{name}: output i");
            let s_out = c.s_out(n, c.tp_price).unwrap();
            let next = CondPairState {
                amount_left: c.amount_left - n,
                custody: c.custody - s_out,
                bought: if mode == MODE_ACCUMULATE { t } else { 0 },
                ..c.clone()
            };
            assert!(output_with(&s, &next.spk()).is_some(), "{name}: the continuation's bought");
        }
    }
}

/// The builder's mode-1 rules are the covenant's: the same transaction with the continuation's `bought` left at 0, or with
/// output i paid to the maker's key while the order continues, is refused by the order's input.
#[test]
fn the_mode_one_continuation_is_the_covenants() {
    use common::exact::{build_measured, patched_outcomes};
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let x = pair_mode(pair(MAKER_A, false, pa, pb, RATE), MODE_ACCUMULATE);
    let n = 4 * WHOLE;
    let built = build_measured(&Action::Batch(route_bid(x.clone(), pa, pb, 4))).unwrap();
    let (st, me) = (x.encode(), spk_of(&x.spk()));
    let ok = patched_outcomes(built.clone(), 0, st.clone(), me.clone(), &[], &[]);
    assert!(ok.iter().all(|r| r.is_ok()), "control {ok:?}");
    let f = x.fill(n, x.price, pv() as i64).unwrap();
    let next = PairState { amount_left: x.amount_left - n, custody: f.out_amount, ..x.clone() };
    let stale = vec![(spk_of(&PairState { bought: n, ..next.clone() }.spk()), spk_of(&next.spk()))];
    let bad = patched_outcomes(built.clone(), 0, st.clone(), me.clone(), &stale, &[]);
    assert!(bad[0].is_err(), "a continuation with a stale bought: {bad:?}");
    let (ext, tpl) = (ext_for(pa), kob_protocol::artifacts::token_template(pa));
    let plain = vec![(
        spk_of(&TokenState::custody(pa.family(), n, Y_ID, ext).spk_with(tpl)),
        spk_of(&TokenState::user(pa, n, pk(MAKER_A), ext).spk_with(tpl)),
    )];
    let bad = patched_outcomes(built, 0, st, me, &plain, &[]);
    assert!(bad[0].is_err(), "a continuing mode-1 order paying the maker's key: {bad:?}");
}

/// A KRON delivery is mode 0: a pair order buying a KRON token in mode 1 or 2 is refused at placement and at fill time.
#[test]
fn a_kron_delivery_is_mode_zero() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::KronToken2433);
    for mode in [MODE_ACCUMULATE, MODE_TOPUP] {
        let x = pair_mode(pair(MAKER_A, true, pa, pb, RATE), mode);
        assert!(kob_protocol::build::check_new_order(&AnyState::KobPair(x.clone())).is_err(), "placement, mode {mode}");
        let e = common::exact::build_measured(&Action::Batch(route_ask(x, pa, pb, 4))).unwrap_err().to_string();
        assert!(e.contains("KRON delivery is mode 0"), "mode {mode}: {e}");
    }
    let c = cond_mode(cond_pair_ask(MAKER_A, pa, pb), MODE_ACCUMULATE);
    assert!(kob_protocol::build::check_new_order(&AnyState::KobCondPair(c)).is_err());
}

/// The placement rules of the delivery block (bid-delivery-spec §2) and the funding of a mode-1 order: one carrier, since
/// only its first fill puts a carrier on the bought-custody.
#[test]
fn placement_checks_the_delivery_block() {
    use kob_protocol::build::{check_new_order, min_order_value};
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let x = pair(MAKER_A, true, pa, pb, RATE);
    let ok = |s: PairState| check_new_order(&AnyState::KobPair(s));
    assert!(ok(pair_mode(x.clone(), MODE_ACCUMULATE)).is_ok());
    assert!(ok(pair_mode(x.clone(), MODE_TOPUP)).is_ok());
    assert!(ok(PairState { bought: 1, ..pair_mode(x.clone(), MODE_ACCUMULATE) }).is_err(), "a new order has bought 0");
    assert!(ok(PairState { mode: 3, ..x.clone() }).is_err());
    assert!(ok(PairState { mode: -1, ..x.clone() }).is_err());
    assert!(ok(PairState { recv_threshold: 0, ..pair_mode(x.clone(), MODE_TOPUP) }).is_err());
    assert!(ok(PairState { topup_tip: x.delivery_carrier + 1, ..pair_mode(x.clone(), MODE_TOPUP) }).is_err());
    assert!(ok(PairState { recv_threshold: 5, ..x.clone() }).is_err(), "mode 0 has no threshold");
    let c = cond_pair_bid(MAKER_A, pa, pb);
    assert!(check_new_order(&AnyState::KobCondPair(cond_mode(c.clone(), MODE_ACCUMULATE))).is_ok());
    assert!(check_new_order(&AnyState::KobCondPair(CondPairState { bought: 1, ..cond_mode(c, MODE_ACCUMULATE) })).is_err());
    // funding: one carrier in mode 1, a partial fill and its rest otherwise
    let tip = x.tip_kas(x.amount_left).unwrap();
    assert_eq!(min_order_value(&AnyState::KobPair(pair_mode(x.clone(), MODE_ACCUMULATE))), x.delivery_carrier + tip);
    assert_eq!(min_order_value(&AnyState::KobPair(pair_mode(x.clone(), MODE_TOPUP))), 2 * x.delivery_carrier + tip);
    // an entry's exits: mode 1 for a non-repeating entry, mode 0 only for a repeating one
    let i = ifd_pair(MAKER_A, true, pa, pb);
    assert!(check_new_order(&AnyState::KobIfdPair(IfdPairState { exit_mode: MODE_ACCUMULATE, ..i.clone() })).is_ok());
    let rpt = IfdPairState { exit_mode: MODE_ACCUMULATE, rpt_amount: 20 * WHOLE, ..i.clone() };
    let e = check_new_order(&AnyState::KobIfdPair(rpt)).unwrap_err().to_string();
    assert!(e.contains("mode 0"), "{e}");
}

/// A pair entry writes its exit block into the exit it creates (bought = 0): the exit of a fill of 4 whole A, both sides,
/// exit modes 1 and 2.
#[test]
fn a_pair_entry_writes_its_exit_block() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    for buy in [true, false] {
        for mode in [MODE_ACCUMULATE, MODE_TOPUP] {
            let (thr, tip) = if mode == MODE_TOPUP { (RECV_THR, TOPUP_TIP) } else { (0, 0) };
            let i = IfdPairState { exit_mode: mode, exit_recv_threshold: thr, exit_topup_tip: tip, ..ifd_pair(MAKER_A, buy, pa, pb) };
            kob_protocol::build::check_new_order(&AnyState::KobIfdPair(i.clone())).unwrap();
            let b = if buy { ifd_bid_route(i.clone(), pa, pb, 4) } else { ifd_ask_route(i.clone(), pa, pb, 4) };
            let s = run(&format!("entry buy={buy} exit mode {mode}"), &Action::Batch(b));
            let n = 4 * WHOLE;
            let x_cust = if buy { n } else { i.proceeds(n, i.price).unwrap() + i.pre_of(n).unwrap() };
            let exit = i.exit_for(n, x_cust, None).unwrap();
            assert_eq!((exit.mode, exit.recv_threshold, exit.topup_tip, exit.bought), (mode, thr, tip, 0));
            assert!(output_with(&s, &exit.spk()).is_some(), "buy={buy} mode {mode}: the exit carries the entry's exit block");
        }
    }
}

/// A mode-1 order that already holds a bought-custody spends it in every fill: without it the fill is refused rather than
/// built into a transaction the covenant refuses. Modes 0 and 2 never hold one.
#[test]
fn a_held_bought_custody_is_never_left_behind() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let x = PairState { bought: 3 * WHOLE, ..pair_mode(pair(MAKER_A, false, pa, pb, RATE), MODE_ACCUMULATE) };
    let e = common::exact::build_measured(&Action::Batch(route_bid(x, pa, pb, 4))).unwrap_err().to_string();
    assert!(e.contains("bought-custody"), "{e}");
    for mode in [MODE_NEW, MODE_TOPUP] {
        let y = PairState { bought: 1, ..pair_mode(pair(MAKER_A, false, pa, pb, RATE), mode) };
        let e = common::exact::build_measured(&Action::Batch(route_bid(y, pa, pb, 4))).unwrap_err().to_string();
        assert!(e.contains("bought"), "mode {mode}: {e}");
    }
}

// -------- the delivery inputs of a leg: a bought-custody, a receive UTXO, a form
//
// `Leg::Pair` / `Leg::CondPair` take them as `boughtCustody`, `receive` and `form`; `RefundOrder` takes `boughtCustody`.
// The legs below are written through serde, and `with_delivery` checks the library keeps the fields.

const NEEDS_PATCH: &str = "the Leg / RefundOrder delivery fields";

/// `leg` with its delivery inputs, through the request JSON. Panics if the library drops them.
fn with_delivery(leg: Leg, bought: Option<&TokenUtxo>, receive: Option<&TokenUtxo>, form: Option<u8>) -> Leg {
    let mut v = serde_json::to_value(&leg).unwrap();
    if let Some(b) = bought {
        v["boughtCustody"] = serde_json::to_value(b).unwrap();
    }
    if let Some(r) = receive {
        v["receive"] = serde_json::to_value(r).unwrap();
    }
    if let Some(f) = form {
        v["form"] = serde_json::json!(f);
    }
    let out: Leg = serde_json::from_value(v.clone()).unwrap();
    let back = serde_json::to_value(&out).unwrap();
    assert_eq!(back.get("boughtCustody"), v.get("boughtCustody"), "{NEEDS_PATCH}");
    assert_eq!(back.get("form"), v.get("form"), "{NEEDS_PATCH}");
    out
}

/// The KAS on a bought-custody of the fixtures.
const BOUGHT_KAS: u64 = 3 * KAS;

/// A bought-custody of `b` units of T for the order of covenant `c`.
fn bought_custody(tp: TemplateId, tok: [u8; 32], c: [u8; 32], b: i64) -> TokenUtxo {
    TokenUtxo { utxo: utxo(0xb7, BOUGHT_KAS, 1_000, Some(tok)), state: TokenState::custody(tp.family(), b, c, ext_for(tp)) }
}

/// Mode 1 once the bought-custody exists: a later partial fill grows it to bought + n at output i with its own KAS (no
/// carrier leaves the order) and the continuation records bought + n; the fill that ends the order pays bought + n to the
/// maker's key with every KAS of the order and of the custody. Both sides, `KobPair` and `KobCondPair`.
#[test]
fn mode_one_spends_and_continues_the_bought_custody() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let b = 3 * WHOLE;
    for ask in [true, false] {
        let (tok, tp) = t_of(ask, pa, pb);
        let ext = ext_for(tp);
        let cov = if ask { X_ID } else { Y_ID };
        let x = PairState { bought: b, ..pair_mode(pair(MAKER_A, ask, pa, pb, RATE), MODE_ACCUMULATE) };
        let n = 4 * WHOLE;
        let t = x.t_out_min(n, x.price).unwrap();
        let bc = bought_custody(tp, tok, cov, b);
        let mut batch = if ask { route_ask(x.clone(), pa, pb, 4) } else { route_bid(x.clone(), pa, pb, 4) };
        batch.legs[0] = with_delivery(batch.legs[0].clone(), Some(&bc), None, None);
        let s = run(&format!("later ask={ask}"), &Action::Batch(batch));
        assert!(holds(&s, 0, tp, tok, &TokenState::custody(tp.family(), b + t, cov, ext)), "ask={ask}: the custody grows");
        assert_eq!(s.tx.outputs[0].value, BOUGHT_KAS, "ask={ask}: output i carries the custody's KAS, no new carrier");
        let f = x.fill(n, x.price, pv() as i64).unwrap();
        let next = PairState { amount_left: x.amount_left - n, custody: f.out_amount, bought: b + t, ..x.clone() };
        let k = output_with(&s, &next.spk()).expect("the continuation records bought + n");
        assert_eq!(s.tx.outputs[k].value, pv(), "ask={ask}: no carrier leaves the order");
        // the ending fill
        let x4 = PairState { bought: b, ..with_left(x.clone(), 4) };
        let mut batch = if ask { route_ask(x4, pa, pb, 4) } else { route_bid(x4, pa, pb, 4) };
        batch.legs[0] = with_delivery(batch.legs[0].clone(), Some(&bc), None, None);
        let s = run(&format!("end ask={ask}"), &Action::Batch(batch));
        assert!(holds(&s, 0, tp, tok, &TokenState::user(tp, b + t, pk(MAKER_A), ext)), "ask={ask}: bought + n to the maker");
        // a look-alike custody (one unit short) is refused by the builder
        let mut batch = if ask { route_ask(x.clone(), pa, pb, 4) } else { route_bid(x.clone(), pa, pb, 4) };
        batch.legs[0] = with_delivery(batch.legs[0].clone(), Some(&bought_custody(tp, tok, cov, b - 1)), None, None);
        let e = common::exact::build_measured(&Action::Batch(batch)).unwrap_err().to_string();
        assert!(e.contains("bought-custody"), "{e}");
    }
    // KobCondPair, a take-profit sell
    let (tok, tp) = (TOKEN_B, pb);
    let c = CondPairState { bought: b, ..cond_mode(cond_pair_ask(MAKER_A, pa, pb), MODE_ACCUMULATE) };
    let n = 4 * WHOLE;
    let t = c.t_out_min(n, c.tp_price).unwrap();
    let leg = with_delivery(cond_pair_leg(c.clone(), pa, pb, X_ID, 70, 4, 0), Some(&bought_custody(tp, tok, X_ID, b)), None, None);
    let s = run(
        "cond later",
        &Action::Batch(route(vec![
            leg,
            kbid_leg(TOKEN_COV, pa, MAKER_B, P260, cov(0x91), 72, 10, 4),
            kask_leg(TOKEN_B, pb, MAKER_C, P250, cov(0x92), 74, 10, 5),
        ])),
    );
    assert!(holds(&s, 0, tp, tok, &TokenState::custody(tp.family(), b + t, X_ID, ext_for(tp))));
}

/// The refund of a mode-1 order holding a bought-custody releases it whole, with its KAS, to the maker at its own input
/// index (input 1, before the sell custody): `KobPair` and `KobCondPair`. Without it the builder refuses.
#[test]
fn a_refund_releases_the_bought_custody() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let b = 3 * WHOLE;
    let refund_with = |s: AnyState, custody: TokenUtxo, bc: Option<&TokenUtxo>| {
        let mut v = serde_json::to_value(RefundOrder {
            order: order(70, pv(), X_ID, 1_000, s),
            foreign: vec![],
            custody: Some(custody),
            prefund: None,
            bought_custody: None,
            lock_time: EXPIRY as u64,
            funding: vec![key_utxo(250, KEEPER, 5 * KAS)],
            change: Some(pk(KEEPER)),
            fee: fee(),
        })
        .unwrap();
        if let Some(c) = bc {
            v["boughtCustody"] = serde_json::to_value(c).unwrap();
        }
        Action::RefundOrder(serde_json::from_value(v).unwrap())
    };
    for ask in [true, false] {
        let (tok, tp) = t_of(ask, pa, pb);
        let (stok, sp) = s_of(ask, pa, pb);
        let x = PairState { bought: b, ..pair_mode(pair(MAKER_A, ask, pa, pb, RATE), MODE_ACCUMULATE) };
        let custody = tutxo(sp, stok, 71, x.custody, X_ID, true);
        let bc = bought_custody(tp, tok, X_ID, b);
        let s = run(&format!("refund ask={ask}"), &refund_with(AnyState::KobPair(x.clone()), custody.clone(), Some(&bc)));
        assert!(holds(&s, 1, tp, tok, &TokenState::user(tp, b, pk(MAKER_A), ext_for(tp))), "ask={ask}: the release at index 1");
        assert!(s.tx.outputs[1].value >= BOUGHT_KAS);
        assert!(kob_protocol::build::build(&refund_with(AnyState::KobPair(x), custody, None)).is_err(), "the custody is required");
    }
    let c = CondPairState { bought: b, ..cond_mode(cond_pair_ask(MAKER_A, pa, pb), MODE_ACCUMULATE) };
    let custody = tutxo(pa, TOKEN_COV, 71, c.custody, X_ID, true);
    let s = run("refund cond", &refund_with(AnyState::KobCondPair(c), custody, Some(&bought_custody(pb, TOKEN_B, X_ID, b))));
    assert!(holds(&s, 1, pb, TOKEN_B, &TokenState::user(pb, b, pk(MAKER_A), ext_for(pb))));
}

/// Mode 2 form 0 on request: a new plain UTXO; a form above 2 is refused.
#[test]
fn mode_two_takes_the_requested_form() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let x = pair_mode(pair(MAKER_A, false, pa, pb, RATE), MODE_TOPUP);
    let mut batch = route_bid(x.clone(), pa, pb, 4);
    batch.legs[0] = with_delivery(batch.legs[0].clone(), None, None, Some(0));
    let s = run("form 0", &Action::Batch(batch));
    assert!(holds(&s, 0, pa, TOKEN_COV, &TokenState::user(pa, 4 * WHOLE, pk(MAKER_A), ext_for(pa))));
    let mut batch = route_bid(x, pa, pb, 4);
    batch.legs[0] = with_delivery(batch.legs[0].clone(), None, None, Some(3));
    assert!(common::exact::build_measured(&Action::Batch(batch)).is_err());
}

/// Mode 2 form 2: the fill tops up the maker's receive UTXO (T's first input, the leader on the borrow path) to have + n
/// at output i, both sides, in the engine. A receive UTXO below the order's `recvThreshold` is refused by the builder.
#[test]
fn mode_two_tops_up_the_receive_utxo() {
    let (pa, pb) = (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8);
    let have = 7 * WHOLE;
    for ask in [true, false] {
        let (tok, tp) = t_of(ask, pa, pb);
        let ext = ext_for(tp);
        let x = pair_mode(pair(MAKER_A, ask, pa, pb, RATE), MODE_TOPUP);
        let t = x.t_out_min(4 * WHOLE, x.price).unwrap();
        let recv =
            |thr: i64| TokenUtxo { utxo: utxo(0xb8, BOUGHT_KAS, 1_000, Some(tok)), state: receive_state(have, pk(MAKER_A), thr, ext) };
        let mut batch = if ask { route_ask(x.clone(), pa, pb, 4) } else { route_bid(x.clone(), pa, pb, 4) };
        batch.legs[0] = with_delivery(batch.legs[0].clone(), None, Some(&recv(RECV_THR)), Some(2));
        let s = run(&format!("top-up ask={ask}"), &Action::Batch(batch));
        assert!(holds(&s, 0, tp, tok, &receive_state(have + t, pk(MAKER_A), RECV_THR, ext)), "ask={ask}: have + n at output i");
        let mut batch = if ask { route_ask(x.clone(), pa, pb, 4) } else { route_bid(x, pa, pb, 4) };
        batch.legs[0] = with_delivery(batch.legs[0].clone(), None, Some(&recv(RECV_THR - 1)), Some(2));
        assert!(common::exact::build_measured(&Action::Batch(batch)).is_err(), "ask={ask}: a threshold below recvThreshold");
    }
}
