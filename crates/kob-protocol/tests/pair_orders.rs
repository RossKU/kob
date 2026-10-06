//! Pair orders (`KobPair`, `KobCondPair`, `KobIfdPair`; one template each for both sides and both families of A and B),
//! engine-validated through the builders: creation (both custodies of a sell-first entry), cancel (strays of both tokens),
//! refunds and kills, fills routed through the KAS books, netting of opposite pair orders (1 x 1, 2 x 2), conditional fills
//! armed in the fill by evidence (two KAS-book fills, or a resting pair order), updates (arm, trail) in both evidence
//! modes, an armed stop's auction, if-done fills, bookings and merges of both sides, for program pairs of every family mix.
//! Every transaction runs every order covenant and token program in the rusty-kaspa v2.1.0 engine and pays the node's relay
//! floor. Plus the builders' refusals and the maker-value checks (a maker never gets less than its terms).

mod common;

use common::exact::run_measured as run_any;
use common::pair::*;
use common::*;
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{Action, Batch, Leg};
use kob_protocol::state::*;
use kob_protocol::tx::{spk_to_string, SignedTx};

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

/// Builder refusals of what the covenants refuse (and of what the founder rules forbid).
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

/// The layout keeps every positional token output (a rest or a return at its custody's input index) right after the order
/// inputs, whichever token it is of: a transaction whose SECOND token has an IOC return while its first token has several
/// custodies without one used to leave an output slot reserved but never filled.
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
        Ok(_) => common::PROGRAMS.iter().flat_map(|a| common::PROGRAMS.iter().map(move |b| (*a, *b))).collect(),
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
