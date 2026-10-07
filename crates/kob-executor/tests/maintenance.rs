//! Operator maintenance (`kob_executor::maintenance`): the operator's own token UTXOs (earlier routes kept the
//! remainder of their purchases of B there; tokens may still reach the operator's key), a 10 KAS carrier each, are merged,
//! freeing the carriers, or sold into a bid the matcher's tick left when that pays more than the fee; per token, bounded per
//! tick, never touching what pending transactions spend. Every job is engine-validated.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::pair::*;
use common::*;
use kob_executor::maintenance::{tick, MaintKind, MaintReport, MaintenanceConfig, MaintenanceInput, OwnToken};
use kob_executor::matcher::book::{outpoint, Clock, ListedOrder};
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::state::*;
use kob_protocol::tx::*;

const K3: TemplateId = TemplateId::KronToken2433;

/// An operator token UTXO of program `p` holding `amount` base units on a 10 KAS carrier.
fn own(p: TemplateId, amount: i64) -> OwnToken {
    OwnToken {
        program: p,
        token: TokenUtxo { utxo: utxo(CARRIER, 900, Some(token_a(p))), state: tstate(p, amount, pk(MATCHER), false) },
    }
}

fn input(tokens: Vec<OwnToken>, orders: Vec<ListedOrder>, funding: Vec<KeyUtxo>) -> MaintenanceInput {
    MaintenanceInput {
        tokens,
        orders,
        clock: Clock { daa: NOW + 5, utc: UTC },
        funding,
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
    }
}

fn run(inp: &MaintenanceInput, cfg: &MaintenanceConfig) -> MaintReport {
    let r = tick(inp, cfg, &signer());
    for j in &r.jobs {
        assert!(j.validation.is_some(), "every job is engine-validated");
    }
    r
}

/// A resting ask of the operator's token at 2.70 per whole token with 10 whole tokens: the book's price the sale floor
/// (`MaintenanceConfig::sell_floor_bps`, 90 %) compares a bid with. It does not cross the 2.60 bids.
fn market_ask() -> ListedOrder {
    l_ask_a(300, T8, 9, 270_000_000, 10 * WHOLE)
}

/// The token outputs of a job (count), and the carrier of the operator's own one holding `amount` (None: there is none).
fn token_outs(j: &kob_executor::maintenance::MaintJob, p: TemplateId, amount: i64) -> (usize, Option<u64>) {
    let mine = spk_to_string(&tstate(p, amount, pk(MATCHER), false).spk_with(token_template(p)));
    let outs: Vec<_> = j.built.tx.outputs.iter().filter(|o| o.covenant.as_ref().map(|c| c.covenant_id) == Some(token_a(p))).collect();
    (outs.len(), outs.iter().find(|o| o.script_public_key == mine).map(|o| o.value))
}

#[test]
fn dust_of_one_token_is_merged_into_one_utxo_and_the_carriers_come_back() {
    // five route remainders of an 8x8 token: one transaction, one token UTXO, 40 KAS of carriers back in the change
    let tokens: Vec<OwnToken> = [120, 7, 999, 1, 33].into_iter().map(|a| own(T8, a)).collect();
    let r = run(&input(tokens, vec![], vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.kind, MaintKind::Merge);
    assert_eq!(j.spends.len(), 5);
    assert_eq!(token_outs(j, T8, 1_160), (1, Some(CARRIER)));
    assert_eq!(j.released, 4 * CARRIER as i64);
    assert!(j.profit < 0 && j.profit > -(KAS as i64) / 10, "a merge costs only its fee: {}", j.profit);
    let change = j.built.fee.change_output.map(|i| j.built.tx.outputs[i as usize].value).unwrap();
    assert_eq!(change as i64, j.released + j.profit, "the freed carriers less the fee, to the operator");
}

#[test]
fn a_merge_takes_what_the_program_allows_and_a_single_utxo_is_left_alone() {
    // Kcc20Ref: three token inputs per transaction
    let tokens: Vec<OwnToken> = (1..=5).map(|a| own(T3, a)).collect();
    let r = run(&input(tokens, vec![], vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs[0].spends.len(), 3);
    assert_eq!(token_outs(&r.jobs[0], T3, 1 + 2 + 3), (1, Some(CARRIER)), "the oldest first, deterministic");
    let r = run(&input(vec![own(T8, 5)], vec![], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.is_empty());
}

#[test]
fn kron_tokens_need_the_operators_p2pk_input() {
    let tokens = vec![own(K3, 10), own(K3, 20)];
    let r = run(&input(tokens.clone(), vec![], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.is_empty());
    assert!(r.skipped.iter().any(|(_, why)| why.contains("P2PK input")), "{:?}", r.skipped);
    // the smallest funding UTXO is the presence; its change returns it
    let r = run(&input(tokens, vec![], vec![funding(50 * KAS), funding(3 * KAS)]), &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.built.roles.iter().filter(|r| *r == "p2pk").count(), 1);
    assert_eq!(j.built.tx.inputs.iter().filter(|i| i.utxo.amount == 3 * KAS && i.utxo.covenant_id.is_none()).count(), 1);
    assert_eq!(token_outs(j, K3, 30), (1, Some(CARRIER)));
}

#[test]
fn the_operators_tokens_are_sold_into_a_bid_left_after_the_tick() {
    // v3 (no lots): the sale takes any amount the bid's quantity rules accept. 2.5 whole tokens (2,500 base units) in three
    // UTXOs; a resting bid at 2.60 per whole token with a buying power of 2 whole tokens takes 2,000 base units, 500 come back
    // as one UTXO, the bid pays floor(2,000 x (2.60 + tip) / 1,000) = 5.2 KAS (+ its tips) to the operator
    let tokens = vec![own(T8, 1_200), own(T8, 800), own(T8, 500)];
    let bid = l_bid_a(100, T8, 2, P260, 2 * WHOLE);
    let r = run(&input(tokens.clone(), vec![bid, market_ask()], vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.kind, MaintKind::Sell, "{:?}", r.skipped);
    assert_eq!(j.order, Some(cid(100)));
    assert_eq!(token_outs(j, T8, 500), (2, Some(CARRIER)), "the bid's delivery and the operator's rest");
    assert_eq!(j.released, 2 * CARRIER as i64);
    assert!(j.profit > 2 * P260 - KAS as i64 / 10 && j.profit <= 2 * P260 + 2 * TIP, "proceeds less the fee: {}", j.profit);
    // a bid with room for 4 whole tokens takes all 2,500 base units (no whole-lot remainder any more): one token output, the
    // bid's delivery, and every carrier comes back
    let r = run(&input(tokens, vec![l_bid_a(100, T8, 2, P260, 4 * WHOLE), market_ask()], vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    let j = &r.jobs[0];
    assert_eq!(j.kind, MaintKind::Sell, "{:?}", r.skipped);
    assert_eq!(token_outs(j, T8, 2_500), (1, None), "only the bid's delivery");
    assert_eq!(j.released, 3 * CARRIER as i64);
    let pays = 2_500 * (P260 + TIP) / WHOLE; // exact: 2,500 x 260,100,000 is a multiple of 1,000
    assert!(j.profit > pays - KAS as i64 / 10 && j.profit <= pays, "proceeds less the fee: {}", j.profit);
    // selling off: the same dust is merged instead
    let cfg = MaintenanceConfig { sell: false, ..Default::default() };
    let r = run(&input(vec![own(T8, 1_200), own(T8, 800)], vec![l_bid_a(100, T8, 2, P260, 4 * WHOLE)], vec![]), &cfg);
    assert_eq!(r.jobs[0].kind, MaintKind::Merge);
}

#[test]
fn a_sale_that_does_not_pay_its_fee_is_a_merge() {
    // a bid of 1 sompi per whole token: the sale would lose to the fee, so the UTXOs are merged
    let tokens = vec![own(T8, 1_000), own(T8, 1_000)];
    let mut b = bid(2, 1, T8);
    b.token_cov_id = token_a(T8);
    let low = ListedOrder {
        family: kob_protocol::family::Family::Kcc20,
        order: OrderUtxo {
            utxo: utxo((b.used(4 * WHOLE).unwrap() + b.delivery_carrier + b.reserve) as u64, 1_000, Some(cid(101))),
            state: AnyState::KobBid(b),
        },
        custody: None,
        custody_b: None,
        deadline: None,
        seen_daa: 1_000,
        foreign: vec![],
        strays: vec![],
    };
    // (no price floor here: this is the fee rule alone)
    let cfg = MaintenanceConfig { sell_floor_bps: 0, ..Default::default() };
    let r = run(&input(tokens, vec![low], vec![]), &cfg);
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    assert_eq!(r.jobs[0].kind, MaintKind::Merge);
    assert!(r.skipped.iter().any(|(_, why)| why.starts_with("sell:")), "{:?}", r.skipped);
}

#[test]
fn bounded_per_tick_and_nothing_a_pending_transaction_spends() {
    let mut tokens: Vec<OwnToken> = vec![own(T8, 1), own(T8, 2)];
    // a second token of the same program (another covenant id) and a third
    for (t, n) in [([0x74u8; 32], 3), ([0x75u8; 32], 4)] {
        for k in 0..n {
            let mut o = own(T8, 10 + k);
            o.token.utxo.covenant_id = Some(t);
            tokens.push(o);
        }
    }
    let cfg = MaintenanceConfig { max_jobs: 2, ..Default::default() };
    let r = run(&input(tokens.clone(), vec![], vec![]), &cfg);
    assert_eq!(r.jobs.len(), 2);
    // the outpoints pending transactions spend are left alone: one UTXO of the first token is not enough for a merge
    let mut inp = input(tokens, vec![], vec![]);
    inp.excluded_outpoints.insert(outpoint(&inp.tokens[0].token.utxo));
    let r = run(&inp, &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 2);
    assert!(r.jobs.iter().all(|j| j.token != token_a(T8)));
    // a bid a pending transaction fills is not sold into either
    let mut inp = input(vec![own(T8, 1_000), own(T8, 1_000)], vec![l_bid_a(100, T8, 2, P260, 4 * WHOLE)], vec![]);
    inp.excluded.insert(cid(100));
    assert_eq!(run(&inp, &MaintenanceConfig::default()).jobs[0].kind, MaintKind::Merge);
    // and nothing at all when switched off
    let off = MaintenanceConfig { enabled: false, ..Default::default() };
    assert!(run(&input(vec![own(T8, 1), own(T8, 2)], vec![], vec![]), &off).jobs.is_empty());
}

#[test]
fn tokens_of_other_owners_or_in_custody_are_never_touched() {
    let mut other = own(T8, 5);
    other.token.state = tstate(T8, 5, pk(2), false);
    let mut custody = own(T8, 6);
    custody.token.state = tstate(T8, 6, pk(MATCHER), true);
    let r = run(&input(vec![own(T8, 1), other, custody], vec![], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.is_empty(), "one own UTXO only");
}

#[test]
fn maintenance_pays_the_low_rate_and_a_sale_only_what_it_earns() {
    let fees = |low: u64| kob_executor::fee::FeeRates { floor: 100, low, normal: 500, high: 900, max_tx_fee: 0, estimated: true };
    // a merge: housekeeping, the low rate
    let cfg = MaintenanceConfig { sell: false, fees: fees(130), ..Default::default() };
    let r = run(&input(vec![own(T8, 1_200), own(T8, 800)], vec![], vec![]), &cfg);
    assert_eq!(r.jobs[0].kind, MaintKind::Merge, "{:?}", r.skipped);
    assert_eq!(r.jobs[0].built.fee.fee_rate, 130);
    // the default: the floor
    let r = run(&input(vec![own(T8, 1_200), own(T8, 800)], vec![], vec![]), &MaintenanceConfig { sell: false, ..Default::default() });
    assert_eq!(r.jobs[0].built.fee.fee_rate, 100);
    // a sale at a low rate it pays easily
    let cfg = MaintenanceConfig { fees: fees(150), ..Default::default() };
    let bids = vec![l_bid_a(100, T8, 2, P260, 4 * WHOLE), market_ask()];
    let r = run(&input(vec![own(T8, 1_200), own(T8, 800)], bids, vec![]), &cfg);
    assert_eq!((r.jobs[0].kind, r.jobs[0].built.fee.fee_rate), (MaintKind::Sell, 150), "{:?}", r.skipped);
    assert!(r.jobs[0].profit >= 0);
}

/// Surplus inventory (`PlannerConfig::inventory`): the tokens the matcher accumulates are the owner's to sell, off-matcher.
/// Maintenance never sells a listed token, even into a bid that pays well (or a lowball one an attacker posts), whether or
/// not the policy's switch is on; it may still merge its UTXOs. Unlisted tokens are sold as before.
#[test]
fn accumulated_surplus_inventory_is_never_sold() {
    use kob_executor::matcher::planner::{InventoryPolicy, InventoryToken};
    let tokens = vec![own(T8, 1_200), own(T8, 800)];
    let bids = vec![l_bid_a(100, T8, 2, 50_000_000, 4 * WHOLE), l_bid_a(101, T8, 3, P260, 4 * WHOLE), market_ask()];
    // unlisted: sold into the best bid
    let r = run(&input(tokens.clone(), bids.clone(), vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs[0].kind, MaintKind::Sell);
    let listed = InventoryToken { token: token_a(T8), ref_price: None, min_amount: None };
    for on in [true, false] {
        let inventory = InventoryPolicy { accept_surplus_tokens: on, tokens: vec![listed.clone()], ..Default::default() };
        let cfg = MaintenanceConfig { inventory, ..Default::default() };
        let r = run(&input(tokens.clone(), bids.clone(), vec![]), &cfg);
        assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
        assert_eq!(r.jobs[0].kind, MaintKind::Merge, "switch {on}: merged, never sold");
        assert!(r.skipped.iter().any(|(_, why)| why.contains("surplus inventory")), "{:?}", r.skipped);
        // a single UTXO: nothing at all
        let r = run(&input(vec![own(T8, 2_000)], bids.clone(), vec![]), &cfg);
        assert!(r.jobs.is_empty(), "switch {on}");
    }
}

/// A plain resting bid of token A (program 8/8) at `price` sompi per whole token, no tip, buying power `amount` base units.
fn untipped_bid(id: u32, maker: u8, price: i64, amount: i64) -> ListedOrder {
    let mut b = bid(maker, price, T8);
    b.token_cov_id = token_a(T8);
    b.extension_commitment = EXT;
    b.tip = 0;
    let v = (b.used(amount).expect("budget") + b.delivery_carrier + b.reserve) as u64;
    ListedOrder {
        family: kob_protocol::family::Family::Kcc20,
        order: OrderUtxo { utxo: utxo(v, 1_000, Some(cid(id))), state: AnyState::KobBid(b) },
        custody: None,
        custody_b: None,
        deadline: None,
        seen_daa: 1_000,
        foreign: vec![],
        strays: vec![],
    }
}

/// The sale's price floor (`MaintenanceConfig::sell_floor_bps`, default 90 % of what the book's resting asks charge for the
/// same amount): a holding is never sold into a bid far below the market, even when the inventory policy that protects
/// listed tokens is not loaded, and with no asks to price the amount nothing is sold (the UTXOs are merged).
#[test]
fn a_sale_never_goes_below_the_price_floor() {
    // 1,000 whole tokens (2,600 KAS at 2.60) in two UTXOs; a bid at 0.001 KAS per whole token with room for all of it
    let held = vec![own(T8, 500 * WHOLE), own(T8, 500 * WHOLE)];
    let low = untipped_bid(900, 7, 100_000, 1_000 * WHOLE);
    // no asks at all: nothing to price the sale against, merged
    let r = run(&input(held.clone(), vec![low.clone()], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.iter().all(|j| j.kind == MaintKind::Merge), "{:?}", r.skipped);
    assert!(r.skipped.iter().any(|(_, why)| why.starts_with("sell: no resting asks")), "{:?}", r.skipped);
    // asks at the market (2.70, 10 whole): too few to price 1,000 whole tokens, merged
    let r = run(&input(held.clone(), vec![low.clone(), market_ask()], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.iter().all(|j| j.kind == MaintKind::Merge), "{:?}", r.skipped);
    // asks deep enough at the market: the low bid is far below 90 % of their price, merged
    let deep = l_ask_a(301, T8, 9, 270_000_000, 2_000 * WHOLE);
    let r = run(&input(held.clone(), vec![low.clone(), deep.clone()], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.iter().all(|j| j.kind == MaintKind::Merge), "{:?}", r.skipped);
    assert!(r.skipped.iter().any(|(_, why)| why.contains("below 9000 bps")), "{:?}", r.skipped);
    // a bid at 2.50 (93 % of the asks' 2.70) with room for all of it: sold
    let fair = untipped_bid(901, 8, 250_000_000, 1_000 * WHOLE);
    let r = run(&input(held.clone(), vec![low.clone(), fair, deep.clone()], vec![]), &MaintenanceConfig::default());
    assert_eq!(r.jobs.len(), 1, "{:?}", r.skipped);
    assert_eq!((r.jobs[0].kind, r.jobs[0].order), (MaintKind::Sell, Some(cid(901))));
    // a bid at 2.40 (89 %): merged
    let under = untipped_bid(902, 8, 240_000_000, 1_000 * WHOLE);
    let r = run(&input(held.clone(), vec![under, deep], vec![]), &MaintenanceConfig::default());
    assert!(r.jobs.iter().all(|j| j.kind == MaintKind::Merge), "{:?}", r.skipped);
    // the floor switched off (0): any bid that pays the fee, as before
    let off = MaintenanceConfig { sell_floor_bps: 0, ..Default::default() };
    let r = run(&input(held, vec![low], vec![]), &off);
    assert_eq!((r.jobs[0].kind, r.jobs[0].order), (MaintKind::Sell, Some(cid(900))), "{:?}", r.skipped);
}
