//! Keepers (`docs/spec/matcher.md` §5, §11): IOC / FOK kill in the first block that allows it, soft-expiry and idle
//! refunds, repeating entries, stray handling; never an arm or a trail (protocol v2.6: matchers arm stops inside the
//! batches that fill their evidence, `matcher_scenarios.rs`); the pair kinds (`KobPair`, `KobCondPair`, `KobIfdPair`) are refunded,
//! killed and swept like the KAS kinds, a sell-first entry with both its custodies. Every job is validated through the rusty-kaspa
//! v2.1.0 engine.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::pair::*;
use common::*;
use kob_executor::keepers::{tick, JobKind, KeeperConfig, KeeperInput, KeeperReport};
use kob_executor::matcher::book::{Clock, ListedOrder};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::build::Action;
use kob_protocol::state::*;
use kob_protocol::tx::{spk_to_string, TokenUtxo};

const K3: TemplateId = TemplateId::KronToken2433;

fn keep(orders: Vec<ListedOrder>, daa: u64, cfg: &KeeperConfig) -> KeeperReport {
    keep_with(orders, daa, cfg, true)
}

/// A tick over pair orders: no KRON twin (a pair order names two tokens; the tests run each family pair).
fn keep_pair(orders: Vec<ListedOrder>, daa: u64, cfg: &KeeperConfig) -> KeeperReport {
    keep_with(orders, daa, cfg, false)
}

fn keep_with(orders: Vec<ListedOrder>, daa: u64, cfg: &KeeperConfig, twin: bool) -> KeeperReport {
    let inp = KeeperInput {
        orders,
        clock: Clock { daa, utc: UTC },
        funding: vec![funding(20 * KAS)],
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
        known_unprofitable: BTreeSet::new(),
    };
    let r = tick(&inp, cfg, &signer());
    assert_no_budget_slack();
    for (id, why) in &r.skipped {
        eprintln!("skipped {}: {why}", kob_protocol::json::to_hex(id));
    }
    for j in &r.jobs {
        assert!(j.validation.is_some(), "{:?} not engine-validated", j.kind);
    }
    // The same book in the KRON family: the same jobs (what a keeper does depends on the orders and the
    // clock, not on the token program), each built by the KRON builders and engine-validated.
    if twin && std::env::var_os("KOB_NO_KRON_TWIN").is_none() {
        let twin = KeeperInput {
            orders: inp.orders.iter().map(kron_order).collect(),
            clock: inp.clock,
            funding: inp.funding.clone(),
            excluded: inp.excluded.clone(),
            excluded_outpoints: inp.excluded_outpoints.clone(),
            known_unprofitable: BTreeSet::new(),
        };
        let k = tick(&twin, cfg, &signer());
        for j in &k.jobs {
            assert!(j.validation.is_some(), "KRON {:?} not engine-validated", j.kind);
        }
        let key = |r: &KeeperReport| r.jobs.iter().map(|j| (j.kind as u8, j.order)).collect::<BTreeSet<_>>();
        assert_eq!(key(&k), key(&r), "the KRON twin keeps the same orders the same way (skipped: {:?})", k.skipped);
    }
    r
}

fn kinds(r: &KeeperReport) -> Vec<(JobKind, [u8; 32])> {
    r.jobs.iter().map(|j| (j.kind, j.order)).collect()
}

#[test]
fn ioc_and_fok_are_killed_in_the_first_block_that_allows_it() {
    let mut ioc = ask(1, P250, 5 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    let o = listed(cid(1), AnyState::KobAsk(ioc), CARRIER, 5_000);
    let mut fok = bid(2, P245, T3);
    fok.tif = TIF_FOK;
    fok.active_from = 5_100;
    fok.expiry_daa = NO_EXPIRY;
    let b = listed(cid(2), AnyState::KobBid(fok.clone()), (fok.used(3 * WHOLE).unwrap() + DC) as u64, 5_000);
    let cfg = KeeperConfig::default();
    // One DAA before the kill time: nothing.
    assert!(keep(vec![o.clone()], 5_599, &cfg).jobs.is_empty());
    // At the kill time max(UTXO DAA, activeFrom) + 600.
    let r = keep(vec![o.clone(), b.clone()], 5_600, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(1))]);
    let Action::RefundOrder(req) = &r.jobs[0].action else { panic!() };
    assert_eq!(req.lock_time, 5_600, "the earliest lock time");
    let r = keep(vec![b], 5_700, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(2))]);
    assert!(r.jobs[0].profit > 0, "the refund tip pays the keeper");
}

#[test]
fn expiry_idle_and_repeating_entries_are_refunded() {
    let cfg = KeeperConfig::default();
    // GTD ask at expiry; GTC ask after 90 days idle.
    let gtd = listed(cid(1), AnyState::KobAsk(ask(1, P250, 5 * WHOLE, T3)), CARRIER, EXPIRY as u64 - 1_000);
    let mut g = ask(2, P250, 5 * WHOLE, T3);
    g.expiry_daa = NO_EXPIRY;
    let gtc = listed(cid(2), AnyState::KobAsk(g), CARRIER, 1_000);
    assert!(keep(vec![gtd.clone()], EXPIRY as u64 - 1, &cfg).jobs.is_empty());
    let r = keep(vec![gtd], EXPIRY as u64, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(1))]);
    let r = keep(vec![gtc.clone()], 1_000 + MAX_IDLE as u64 - 1, &cfg);
    assert!(r.jobs.is_empty());
    let r = keep(vec![gtc], 1_000 + MAX_IDLE as u64, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(2))]);
    // Conditional, if-done entries, a repeating sell-first entry with nothing left (close).
    let at = EXPIRY as u64;
    let orders = vec![
        l_cond_ask(3, cond_ask(3, 300_000_000, 200_000_000, 4 * WHOLE, T3), 1_000),
        l_cond_bid(4, cond_bid(4, 200_000_000, 300_000_000, 4 * WHOLE, T3), 1_000),
        l_ifd_bid(5, IfdBidState { rpt_amount: 9 * WHOLE, ..ifd_bid(5, P260, 4 * WHOLE, T3) }, 1_000),
        l_ifd_ask(6, ifd_ask(6, P250, 4 * WHOLE, T3), 1_000),
        listed(
            cid(7),
            AnyState::KobIfdAsk(IfdAskState { amount_left: 0, rpt_amount: 5 * WHOLE, ..ifd_ask(7, P250, 4 * WHOLE, T3) }),
            CARRIER,
            1_000,
        ),
    ];
    let r = keep(orders, at, &cfg);
    let mut k = kinds(&r);
    k.sort();
    assert_eq!(
        k,
        vec![
            (JobKind::Refund, cid(3)),
            (JobKind::Refund, cid(4)),
            (JobKind::Refund, cid(5)),
            (JobKind::Refund, cid(6)),
            (JobKind::Close, cid(7)),
        ]
    );
}

#[test]
fn day_orders_refund_at_expiry_not_at_the_deadline() {
    let cfg = KeeperConfig::default();
    let d = kob_protocol::defaults::day_order(NOW, UTC, None);
    let mut a = ask(1, P250, 5 * WHOLE, T3);
    a.expiry_daa = d.expiry_daa as i64;
    let mut o = listed(cid(1), AnyState::KobAsk(a), CARRIER, NOW - 10);
    o.deadline = Some(d.deadline);
    // Past the UTC deadline (matchers stop) but before expiryDaa: not refundable yet.
    assert!(keep(vec![o.clone()], d.expiry_daa - 1, &cfg).jobs.is_empty());
    assert_eq!(kinds(&keep(vec![o], d.expiry_daa, &cfg)), vec![(JobKind::Refund, cid(1))]);
}

#[test]
fn keepers_never_arm_or_trail_stops_the_matcher_arms_them_in_its_batches() {
    // Unarmed stops and stop entries, a trailing stop whose UTXO is old enough, and resting plain orders at or through
    // every trigger: in v2.6 an `update` needs its evidence filled in the same transaction, so a keeper has nothing to
    // build (the matcher arms them next to the fills, `matcher_scenarios.rs`).
    let cfg = KeeperConfig::default();
    let mut trailing = cond_ask(1, 300_000_000, 200_000_000, 4 * WHOLE, T3);
    trailing.trail_step = 5_000_000;
    trailing.trail_gap = 10_000_000;
    let orders = vec![
        l_cond_ask(1, trailing, 2_000),
        l_cond_ask(2, cond_ask(2, 300_000_000, 240_000_000, 4 * WHOLE, T3), 2_000),
        l_cond_bid(3, cond_bid(3, 200_000_000, 300_000_000, 4 * WHOLE, T3), 2_000),
        l_ifd_bid(4, IfdBidState { entry_stop: 255_000_000, ..ifd_bid(4, P260, 4 * WHOLE, T3) }, 2_000),
        l_ifd_ask(5, IfdAskState { entry_stop: P250, ..ifd_ask(5, P245, 4 * WHOLE, T3) }, 2_000),
        l_ask(6, ask(6, 238_000_000, 5 * WHOLE, T3)),
        l_bid(7, bid(7, 302_000_000, T3), 5 * WHOLE),
    ];
    let r = keep(orders, NOW, &cfg);
    assert!(r.jobs.is_empty(), "no keeper job: {:?}", kinds(&r));
}

#[test]
fn keepers_never_move_strays_of_others_and_sweep_their_own_on_request() {
    let mut o = l_ask(1, ask(1, P250, 5 * WHOLE, T3));
    o.strays = vec![custody(3 * WHOLE, cid(1), 1_500)];
    let r = keep(vec![o], NOW, &KeeperConfig::default());
    assert!(r.jobs.is_empty());
    assert_eq!(r.foreign_strays, vec![cid(1)]);
    // The operator's own orders are swept IN PLACE: the same order continues (same script, covenant id and custody), the
    // stray goes back to the operator. Any kind: a plain ask, a booked if-done exit, a repeating entry.
    let mut own = l_ask(2, ask(MATCHER, P250, 5 * WHOLE, T3));
    own.strays = vec![custody(3 * WHOLE, cid(2), 1_500)];
    let mut entry = l_ifd_bid(3, IfdBidState { rpt_amount: 9 * WHOLE, ..ifd_bid(MATCHER, P260, 4 * WHOLE, T3) }, 1_000);
    entry.strays = vec![custody(2 * WHOLE, cid(3), 1_500)];
    let cfg = KeeperConfig { sweep_own_strays: true, ..KeeperConfig::default() };
    let r = keep(vec![own, entry], NOW, &cfg);
    let mut k = kinds(&r);
    k.sort();
    assert_eq!(k, vec![(JobKind::Sweep, cid(2)), (JobKind::Sweep, cid(3))]);
    for j in &r.jobs {
        let Action::SweepOrder(req) = &j.action else { panic!("{:?}", j.action) };
        assert_eq!(req.strays.len(), 1);
        let tx = &j.built.tx;
        assert_eq!(tx.outputs[0].script_public_key, tx.inputs[0].utxo.script_public_key, "the order continues unchanged");
        assert_eq!(tx.outputs[0].covenant.as_ref().map(|c| c.covenant_id), Some(j.order));
    }
}

/// A token other than the fixture token (another covenant id, the 8/8 program), sent to order `id`.
fn foreign_of(id: [u8; 32], amount: i64) -> kob_protocol::build::ForeignStrays {
    use kob_protocol::build::{ForeignStrays, TokenRef};
    let utxo = kob_protocol::tx::TokenUtxo {
        utxo: kob_protocol::tx::Utxo {
            transaction_id: [0x7c; 32],
            index: 3,
            amount: CARRIER,
            block_daa_score: 1_500,
            covenant_id: Some([0x72; 32]),
        },
        state: Kcc20State::custody(amount, id, EXT).into(),
    };
    ForeignStrays {
        token: TokenRef { covenant_id: [0x72; 32], program: kob_protocol::artifacts::TemplateId::Kcc20Ref8x8 },
        utxos: vec![utxo],
    }
}

#[test]
fn foreign_strays_return_to_the_maker_with_the_refund_on_request() {
    let mut gtd = listed(cid(1), AnyState::KobAsk(ask(1, P250, 5 * WHOLE, T3)), CARRIER, EXPIRY as u64 - 1_000);
    gtd.foreign = vec![foreign_of(cid(1), 77)];
    let mut b = l_bid(2, bid(2, P245, T3), 3 * WHOLE);
    b.order.utxo.block_daa_score = EXPIRY as u64 - 1_000;
    b.foreign = vec![foreign_of(cid(2), 78)];
    // off by default: the refund leaves them (they are lost when the order ends)
    let r = keep(vec![gtd.clone(), b.clone()], EXPIRY as u64, &KeeperConfig::default());
    assert_eq!(r.jobs.len(), 2);
    for j in &r.jobs {
        let Action::RefundOrder(req) = &j.action else { panic!() };
        assert!(req.foreign.is_empty());
    }
    // on request: each refund carries its order's foreign strays to the maker (engine-validated), and still pays the keeper
    let cfg = KeeperConfig { return_foreign_strays: true, ..KeeperConfig::default() };
    let r = keep(vec![gtd, b], EXPIRY as u64, &cfg);
    assert_eq!(r.jobs.len(), 2, "{:?}", r.skipped);
    for j in &r.jobs {
        let Action::RefundOrder(req) = &j.action else { panic!() };
        assert_eq!(req.foreign.len(), 1, "{:?}", j.order);
        assert!(j.profit >= 0);
        let maker = j.built.plans.iter().find_map(|p| match p {
            kob_protocol::tx::SigPlan::TokenLeader { next_states, state, .. } if state.amount == 77 || state.amount == 78 => {
                Some(next_states[0].clone())
            }
            _ => None,
        });
        let s = maker.expect("the foreign token's transfer");
        assert!(s.owner == pk(1) || s.owner == pk(2), "the maker receives it, not the keeper");
    }
}

#[test]
fn a_refund_that_loses_money_only_through_its_funding_is_built_without_it() {
    // the fees of the bid's refund with and without a funding input
    let probe = |tip: i64, funded: bool| {
        let mut s = bid(1, P245, T3);
        s.refund_tip = tip;
        let mut o = l_bid(1, s, 3 * WHOLE);
        o.order.utxo.block_daa_score = EXPIRY as u64 - 1_000;
        let inp = KeeperInput {
            orders: vec![o],
            clock: Clock { daa: EXPIRY as u64, utc: UTC },
            funding: if funded { vec![funding(20 * KAS)] } else { vec![] },
            excluded: BTreeSet::new(),
            excluded_outpoints: BTreeSet::new(),
            known_unprofitable: BTreeSet::new(),
        };
        tick(&inp, &KeeperConfig::default(), &signer())
    };
    let bare = probe(10 * KAS as i64, false);
    let with = probe(10 * KAS as i64, true);
    let (fee_bare, fee_with) = (bare.jobs[0].built.fee.fee as i64, with.jobs[0].built.fee.fee as i64);
    assert!(fee_with > fee_bare, "a funding input costs fee: {fee_with} vs {fee_bare}");
    // a tip between the two: the funded job would lose money, the bare one pays its fee from the tip
    let tip = (fee_bare + fee_with) / 2;
    let r = probe(tip, true);
    assert_eq!(r.jobs.len(), 1, "skipped {:?}", r.skipped);
    assert!(r.unprofitable.is_empty());
    let j = &r.jobs[0];
    let Action::RefundOrder(req) = &j.action else { panic!() };
    assert!(req.funding.is_empty(), "built without the funding input");
    assert!(j.profit >= 0);
}

#[test]
fn unprofitable_jobs_are_skipped_on_request() {
    let mut a = ask(1, P250, 5 * WHOLE, T3);
    a.refund_tip = 0;
    let o = listed(cid(1), AnyState::KobAsk(a), CARRIER, 1_000);
    let r = keep(vec![o.clone()], EXPIRY as u64, &KeeperConfig { min_profit: 1, ..KeeperConfig::default() });
    assert!(r.jobs.is_empty());
    assert_eq!(r.skipped.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// fee policy (`kob_executor::fee`): kills at the high rate, refunds at the normal one, closes at the low one, each within
// what its refund tip pays

fn priced(low: u64, normal: u64, high: u64) -> KeeperConfig {
    KeeperConfig {
        fees: kob_executor::fee::FeeRates { floor: 100, low, normal, high, max_tx_fee: 0, estimated: true },
        ..KeeperConfig::default()
    }
}

fn rate(j: &kob_executor::keepers::Job) -> u64 {
    assert!(j.built.fee.fee >= j.built.fee.fee_rate * j.built.fee.mass.fee_mass);
    j.built.fee.fee_rate
}

#[test]
fn each_job_pays_the_rate_of_its_urgency() {
    let cfg = priced(120, 200, 300);
    let mut ioc = ask(1, P250, 5 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    let kill = listed(cid(1), AnyState::KobAsk(ioc), CARRIER, 5_000);
    let r = keep(vec![kill], 5_600, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(1))]);
    assert_eq!(rate(&r.jobs[0]), 300, "a kill: the high rate");
    assert!(r.jobs[0].profit >= 0);
    let gtd = listed(cid(2), AnyState::KobAsk(ask(2, P250, 5 * WHOLE, T3)), CARRIER, EXPIRY as u64 - 1_000);
    let close = listed(
        cid(7),
        AnyState::KobIfdAsk(IfdAskState { amount_left: 0, rpt_amount: 5 * WHOLE, ..ifd_ask(7, P250, 4 * WHOLE, T3) }),
        CARRIER,
        1_000,
    );
    let r = keep(vec![gtd, close], EXPIRY as u64, &cfg);
    let by: std::collections::BTreeMap<_, _> = r.jobs.iter().map(|j| (j.kind, rate(j))).collect();
    assert_eq!(by.get(&JobKind::Refund), Some(&200), "a refund: the normal rate");
    assert_eq!(by.get(&JobKind::Close), Some(&120), "a close: the low rate");
    // the default configuration: the floor everywhere, as before the policy
    let r = keep(
        vec![listed(cid(3), AnyState::KobAsk(ask(3, P250, 5 * WHOLE, T3)), CARRIER, 1_000)],
        EXPIRY as u64,
        &KeeperConfig::default(),
    );
    assert_eq!(rate(&r.jobs[0]), 100);
}

#[test]
fn a_kill_the_tip_cannot_pay_at_the_high_rate_goes_at_the_highest_rate_it_pays() {
    let mut ioc = ask(1, P250, 5 * WHOLE, T3);
    ioc.tif = TIF_IOC;
    ioc.expiry_daa = NO_EXPIRY;
    let kill = listed(cid(1), AnyState::KobAsk(ioc), CARRIER, 5_000);
    let r = keep(vec![kill.clone()], 5_600, &priced(100, 100, 1_000_000));
    assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(1))], "{:?}", r.skipped);
    let j = &r.jobs[0];
    let rate = rate(j);
    let mass = j.built.fee.mass.fee_mass as i64;
    assert!(rate > 100 && rate < 1_000_000, "rate {rate}");
    assert!(
        j.profit >= 0 && j.profit < 3 * mass,
        "the tip pays the fee with nothing much left: profit {} at {rate} (mass {mass})",
        j.profit
    );
    assert!(r.unprofitable.is_empty());
    // a tip that does not pay even the floor: unprofitable as before (the floor is what it is judged at)
    let mut poor = ask(2, P250, 5 * WHOLE, T3);
    poor.tif = TIF_IOC;
    poor.expiry_daa = NO_EXPIRY;
    poor.refund_tip = 1_000;
    let poor = listed(cid(2), AnyState::KobAsk(poor), CARRIER, 5_000);
    let r = keep(vec![poor], 5_600, &KeeperConfig { min_profit: 0, ..priced(100, 100, 1_000_000) });
    assert!(r.jobs.is_empty(), "{:?}", r.jobs.iter().map(|j| (j.kind, j.profit)).collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------------------------
// pair orders (KobPair / KobCondPair / KobIfdPair): refund, kill and close jobs, engine-validated; no KRON twin (a pair
// order names two tokens, each family pair is a case of its own)

/// The family pairs the pair-kind keeper tests run on (A, B).
const FAMILY_PAIRS: [(TemplateId, TemplateId); 5] = [(T8, T8), (T3, T8), (T3, K3), (K3, T3), (K3, K3)];

/// Whether a job's transaction pays `amount` base units of the token of program `p` to key `maker` (a refund returns every
/// custody to the maker, each token in its own output at the custody's index).
fn pays_token(j: &kob_executor::keepers::Job, p: TemplateId, amount: i64, maker: u8) -> bool {
    let spk = spk_to_string(&tstate(p, amount, pk(maker), false).spk_with(token_template(p)));
    j.built.tx.outputs.iter().any(|o| o.script_public_key == spk)
}

#[test]
fn a_pair_ioc_or_fok_is_killed_in_the_first_block_that_allows_it_and_its_custody_goes_back() {
    let cfg = KeeperConfig::default();
    for (pa, pb) in FAMILY_PAIRS {
        let name = format!("{} / {}", pa.name(), pb.name());
        // an IOC ask (custody: its A) and a FOK bid (custody: its B escrow), neither of them expiring
        let ioc = PairState { expiry_daa: NO_EXPIRY, ..pair_state(1, true, pa, pb, 5 * WHOLE, RATE, PTIP, TIF_IOC) };
        let fok =
            PairState { expiry_daa: NO_EXPIRY, active_from: 5_100, ..pair_state(2, false, pa, pb, 3 * WHOLE, RATE, PTIP, TIF_FOK) };
        let escrow = fok.custody;
        let a = l_pair(1, pa, pb, ioc, 5_000);
        let b = l_pair(2, pa, pb, fok, 5_000);
        // one DAA before the kill time: nothing
        assert!(keep_pair(vec![a.clone(), b.clone()], 5_599, &cfg).jobs.is_empty(), "{name}");
        // at max(UTXO DAA, activeFrom) + 600: the IOC ask only (the FOK bid is active from 5,100)
        let r = keep_pair(vec![a.clone(), b.clone()], 5_600, &cfg);
        assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(1))], "{name}: {:?}", r.skipped);
        let Action::RefundOrder(req) = &r.jobs[0].action else { panic!() };
        assert_eq!(req.lock_time, 5_600, "the earliest lock time");
        assert!(req.prefund.is_none());
        assert!(r.jobs[0].profit > 0, "the refund tip pays the keeper");
        assert!(pays_token(&r.jobs[0], pa, 5 * WHOLE, 1), "{name}: the ask's A goes back to its maker");
        let r = keep_pair(vec![b], 5_700, &cfg);
        assert_eq!(kinds(&r), vec![(JobKind::Kill, cid(2))], "{name}: {:?}", r.skipped);
        assert!(pays_token(&r.jobs[0], pb, escrow, 2), "{name}: the bid's whole B escrow goes back to its maker");
        assert!(r.jobs[0].profit > 0);
    }
}

#[test]
fn a_gtc_pair_order_is_refunded_after_its_expiry_or_90_days_idle() {
    let cfg = KeeperConfig::default();
    for (pa, pb) in [(T8, T8), (T3, K3), (K3, T3)] {
        let name = format!("{} / {}", pa.name(), pb.name());
        let daa = EXPIRY as u64 - 1_000;
        let ask_o = l_pair(1, pa, pb, pask(1, pa, pb, 5 * WHOLE, RATE, PTIP), daa);
        let bid_state = pbid(2, pa, pb, 5 * WHOLE, RATE, PTIP);
        let escrow = bid_state.custody;
        let bid_o = l_pair(2, pa, pb, bid_state, daa);
        let cond = l_cond_pair(3, pa, pb, cond_pair(3, true, pa, pb, 4 * WHOLE, RATE + 200, RATE - 100), daa);
        let cond_buy = l_cond_pair(4, pa, pb, cond_pair(4, false, pa, pb, 4 * WHOLE, RATE - 200, RATE + 100), daa);
        let all = vec![ask_o, bid_o, cond, cond_buy];
        assert!(keep_pair(all.clone(), EXPIRY as u64 - 1, &cfg).jobs.is_empty(), "{name}: not yet");
        let r = keep_pair(all, EXPIRY as u64, &cfg);
        let mut k = kinds(&r);
        k.sort();
        assert_eq!(
            k,
            (1..=4).map(|i| (JobKind::Refund, cid(i))).collect::<Vec<_>>(),
            "{name}: every kind of pair order is refunded (skipped: {:?})",
            r.skipped
        );
        for j in &r.jobs {
            assert!(j.profit > 0, "{name}: the refund tip pays the keeper");
            // each custody returns to its maker in the token it holds: asks and sell stops A, bids and buy stops B
            let sells_a = j.order == cid(1) || j.order == cid(3);
            let maker = if j.order == cid(1) {
                1
            } else if j.order == cid(2) {
                2
            } else if j.order == cid(3) {
                3
            } else {
                4
            };
            if sells_a {
                let n = if j.order == cid(1) { 5 * WHOLE } else { 4 * WHOLE };
                assert!(pays_token(j, pa, n, maker), "{name}: order {maker} gets its A back");
            } else if j.order == cid(2) {
                assert!(pays_token(j, pb, escrow, maker), "{name}: the bid gets its B escrow back");
            }
        }
        // GTC with no expiry: 90 days idle
        let mut idle = pask(1, pa, pb, 5 * WHOLE, RATE, PTIP);
        idle.expiry_daa = NO_EXPIRY;
        let o = l_pair(1, pa, pb, idle, 1_000);
        assert!(keep_pair(vec![o.clone()], 1_000 + MAX_IDLE as u64 - 1, &cfg).jobs.is_empty(), "{name}");
        let r = keep_pair(vec![o], 1_000 + MAX_IDLE as u64, &cfg);
        assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(1))], "{name}: {:?}", r.skipped);
    }
}

#[test]
fn a_sell_first_pair_entry_is_refunded_with_both_its_custodies_a_buy_first_one_with_its_escrow() {
    let cfg = KeeperConfig::default();
    for (pa, pb) in FAMILY_PAIRS {
        let name = format!("{} / {}", pa.name(), pb.name());
        let at = EXPIRY as u64;
        // sell-first: the A custody (amountLeft) and the B prefund custody, two custody inputs, each paid back to the maker
        let sell = ifd_pair(1, false, pa, pb, 4 * WHOLE, RATE);
        let prefund = sell.custody;
        assert!(prefund > 0, "a sell-first entry holds a B prefund");
        let r = keep_pair(vec![l_ifd_pair(1, pa, pb, sell, 1_000)], at, &cfg);
        assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(1))], "{name}: {:?}", r.skipped);
        let Action::RefundOrder(req) = &r.jobs[0].action else { panic!() };
        assert!(req.custody.is_some() && req.prefund.is_some(), "{name}: both custodies are named by the refund");
        assert!(pays_token(&r.jobs[0], pa, 4 * WHOLE, 1), "{name}: the A custody goes back");
        assert!(pays_token(&r.jobs[0], pb, prefund, 1), "{name}: the B prefund goes back");
        assert!(r.jobs[0].profit > 0);
        // buy-first: the B escrow only
        let buy = ifd_pair(2, true, pa, pb, 4 * WHOLE, RATE);
        let escrow = buy.custody;
        let r = keep_pair(vec![l_ifd_pair(2, pa, pb, buy, 1_000)], at, &cfg);
        assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(2))], "{name}: {:?}", r.skipped);
        let Action::RefundOrder(req) = &r.jobs[0].action else { panic!() };
        assert!(req.custody.is_some() && req.prefund.is_none());
        assert!(pays_token(&r.jobs[0], pb, escrow, 2), "{name}: the escrow goes back");
    }
}

/// A repeating sell-first entry with nothing of A left holds only its B prefund: it is refunded too (the budget table covers
/// this shape since b2559da: 95,362 script units on KCC20Ref+KCC20Ref_8x8).
#[test]
fn a_repeating_sell_first_entry_holding_only_its_prefund_is_refunded() {
    let cfg = KeeperConfig::default();
    for (pa, pb) in FAMILY_PAIRS {
        let name = format!("{} / {}", pa.name(), pb.name());
        let empty = IfdPairState { amount_left: 0, rpt_amount: 5 * WHOLE, ..ifd_pair(3, false, pa, pb, 4 * WHOLE, RATE) };
        let held = empty.custody;
        let r = keep_pair(vec![l_ifd_pair(3, pa, pb, empty, 1_000)], EXPIRY as u64, &cfg);
        assert_eq!(r.jobs.len(), 1, "{name}: {:?}", r.skipped);
        assert!(matches!(r.jobs[0].kind, JobKind::Refund | JobKind::Close));
        assert!(pays_token(&r.jobs[0], pb, held, 3), "{name}: the prefund goes back");
    }
}

#[test]
fn a_pair_orders_strays_are_never_moved_by_a_keeper_refund_and_unfunded_tips_are_skipped() {
    // strays of either token on the pair order stay (only the maker's cancel or the operator's sweep moves them)
    let cfg = KeeperConfig::default();
    let mut o = l_pair(1, T8, T3, pask(1, T8, T3, 5 * WHOLE, RATE, PTIP), EXPIRY as u64 - 1_000);
    o.strays = vec![
        TokenUtxo { utxo: utxo(CARRIER, 1_500, Some(token_a(T8))), state: tstate(T8, 3, cid(1), true) },
        TokenUtxo { utxo: utxo(CARRIER, 1_500, Some(token_b(T8, T3))), state: tstate(T3, 4, cid(1), true) },
    ];
    let r = keep_pair(vec![o.clone()], EXPIRY as u64, &cfg);
    assert_eq!(kinds(&r), vec![(JobKind::Refund, cid(1))], "{:?}", r.skipped);
    let spent: BTreeSet<_> = r.jobs[0].built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
    for s in &o.strays {
        assert!(!spent.contains(&(s.utxo.transaction_id, s.utxo.index)), "a stray was spent by a refund");
    }
    // the operator's own pair order is swept in place on request: both tokens' strays go back to the operator
    let mut own = l_pair(2, T8, T3, pask(MATCHER, T8, T3, 5 * WHOLE, RATE, PTIP), 1_000);
    own.strays = o
        .strays
        .iter()
        .map(|s| TokenUtxo {
            state: tstate(if s.utxo.covenant_id == Some(token_a(T8)) { T8 } else { T3 }, 5, cid(2), true),
            ..s.clone()
        })
        .collect();
    let r = keep_pair(vec![own], NOW, &KeeperConfig { sweep_own_strays: true, ..KeeperConfig::default() });
    assert_eq!(kinds(&r), vec![(JobKind::Sweep, cid(2))], "{:?}", r.skipped);
    let Action::SweepOrder(req) = &r.jobs[0].action else { panic!() };
    assert_eq!(req.strays.len(), 2, "a stray of each token");
    // a refund tip that does not pay the fee is not built
    let mut poor = pask(1, T8, T8, 5 * WHOLE, RATE, PTIP);
    poor.refund_tip = 0;
    let r = keep_pair(vec![l_pair(3, T8, T8, poor, 1_000)], EXPIRY as u64, &KeeperConfig { min_profit: 1, ..KeeperConfig::default() });
    assert!(r.jobs.is_empty());
    assert_eq!(r.skipped.len(), 1);
}
