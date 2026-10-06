//! Crossed books at the TN10 soak's real shapes (`tools/soak/config.example.json`): TUSD, TETH and TBTC, all KCC-20 8x8 with 8
//! decimals; TUSD ~23.8 KAS, TETH ~3,000 TUSD, TBTC ~100,000 TUSD; the pair markets TETH/TUSD and TBTC/TUSD (the web's
//! `#/market/TBTC/TUSD`, "BTCUSD") settling through the two KAS books; 5 and 20 USD pair orders (`pairUsd` 3..20), untipped
//! (the web planners' default), wallet-default minimum fills (10 KAS worth; a pair order's: 10 KAS of A); the market maker's
//! ladder of 6 levels per side, 2 bps inside then every 4 bps, at its largest level size.
//!
//! Owner report 2026-10-06: "クロスマッチで、逆ザヤがあってもマッチが動かない" (seen on BTCUSD). Root cause: a netting of crossed
//! pair orders valued its token surplus at the best plain KAS bid without asking whether that bid could take it. A 20 USD pair
//! crossing by 1 % leaves 0.2 TUSD of surplus; every wallet KAS bid has a minimum fill of 10 KAS (~0.42 TUSD), so the surplus
//! could never be sold and went to the pair ask's delivery. The netting was admitted on that phantom income, took both pair
//! orders before the walks, and the batch earned 0 KAS: nothing was built, although the pair bid alone routes through the two
//! KAS books at a profit.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::engine::TickReport;
use kob_protocol::defaults::{default_min_fill, default_min_fill_pair};
use kob_protocol::state::*;

const S8: i64 = 100_000_000;
/// KAS per whole TUSD (sompi): 1 USD at KAS = 0.042 USD.
const KUSD: i64 = 2_380_000_000;

/// An asset token A quoted in TUSD (B): its USD price and the market maker's largest level (base units of A).
#[derive(Clone, Copy)]
struct Asset {
    name: &'static str,
    usd: i64,
    mm_level: i64,
}

const TETH: Asset = Asset { name: "TETH/TUSD", usd: 3_000, mm_level: 300_000 };
const TBTC: Asset = Asset { name: "TBTC/TUSD", usd: 100_000, mm_level: 7_200 };

impl Asset {
    /// The pair rate: TUSD base units per whole A.
    fn rate(self) -> i64 {
        self.usd * S8
    }
    /// KAS per whole A (sompi).
    fn kas(self) -> i64 {
        self.usd * KUSD
    }
    /// `usd` dollars of A (base units).
    fn of(self, usd: i64) -> i64 {
        usd * S8 / self.usd
    }
}

fn bps(p: i64, b: i64) -> i64 {
    (p as i128 * (10_000 + b) as i128 / 10_000) as i64
}

/// A plain KAS ask of `token` (8 decimals) with the wallet's default minimum fill.
fn kas_ask(id: u32, token: [u8; 32], price: i64, amount: i64) -> ListedOrder {
    let mut a = ask(3, price, amount, T8);
    a.token_cov_id = token;
    a.scale = S8;
    a.min_fill = default_min_fill(amount, price, S8);
    a.tip = 0;
    let mut o = l_ask_of(id, token, T8, 3, price, amount);
    o.custody = Some(tcustody(T8, token, a.custody_amount(), cid(id), 1_000));
    o.order.state = AnyState::KobAsk(a);
    o
}

/// A plain KAS bid of `token` (8 decimals) with the wallet's default minimum fill.
fn kas_bid(id: u32, token: [u8; 32], price: i64, amount: i64) -> ListedOrder {
    let mut b = bid(2, price, T8);
    b.token_cov_id = token;
    b.scale = S8;
    b.min_fill = default_min_fill(amount, price, S8);
    b.tip = 0;
    let v = (b.used(amount).expect("budget") + b.delivery_carrier + b.reserve) as u64;
    let mut o = l_bid_of(id, token, T8, 2, price, amount);
    o.order.state = AnyState::KobBid(b);
    o.order.utxo.amount = v;
    o
}

/// A pair order of A/TUSD (both 8 decimals) with the wallet's default pair minimum fill.
fn pair8(a: Asset, id: u32, maker: u8, ask: bool, amount: i64, price: i64, tip: i64) -> ListedOrder {
    let mut s = pair_state(maker, ask, T8, T8, amount, price, tip, TIF_GTC);
    s.s_scale = S8;
    s.t_scale = S8;
    s.min_fill = default_min_fill_pair(amount, Some(a.kas()), S8);
    s.custody = if ask { amount } else { s.bid_escrow(amount, 4).expect("escrow") };
    l_pair(id, T8, T8, s, 1_000)
}

/// The market maker's ladders of A/KAS (ids 100..) and TUSD/KAS (ids 200..): 6 levels per side, 2 bps from the reference
/// then every 4 bps, at the largest level size (TUSD: 6 whole).
fn books(a: Asset) -> Vec<ListedOrder> {
    let mut v = vec![];
    for k in 0..6u32 {
        let d = 2 + 4 * k as i64;
        v.push(kas_bid(100 + k, TOKEN, bps(a.kas(), -d), a.mm_level));
        v.push(kas_ask(110 + k, TOKEN, bps(a.kas(), d), a.mm_level));
        v.push(kas_bid(200 + k, TOKEN_B, bps(KUSD, -d), 6 * S8));
        v.push(kas_ask(210 + k, TOKEN_B, bps(KUSD, d), 6 * S8));
    }
    v
}

fn sum_in(r: &TickReport, ids: std::ops::Range<u32>) -> i64 {
    ids.map(|i| amount_in(r, cid(i))).sum()
}

fn profit(r: &TickReport) -> i64 {
    r.prepared.iter().map(|p| p.accounting.profit).sum()
}

/// The regression: a pair ASK at the reference and a pair BID 20 / 100 bps above it, with both KAS books. Netting them
/// earns the matcher nothing (the surplus is below every KAS bid's minimum fill, so it goes to the ask), but the bid alone
/// routes at a profit: its TUSD sold into the TUSD bids, its A bought from the A asks.
#[test]
fn a_crossed_pair_book_is_matched_when_its_surplus_cannot_be_sold() {
    for a in [TBTC, TETH] {
        for usd in [5, 20] {
            for b in [20, 100] {
                let n = a.of(usd);
                let mut v = vec![pair8(a, 1, 1, true, n, a.rate(), 0), pair8(a, 2, 4, false, n, bps(a.rate(), b), 0)];
                v.extend(books(a));
                let r = run_pair(v, &cfg());
                let tag = format!("{} {usd} USD +{b} bps", a.name);
                assert!(!r.prepared.is_empty(), "{tag}: the crossed pair book is matched");
                assert_eq!(amount_in(&r, cid(2)), n, "{tag}: the pair bid routes whole");
                assert_eq!(sum_in(&r, 110..116), n, "{tag}: exactly its A bought from the A asks");
                assert!(sum_in(&r, 200..206) > 0, "{tag}: its TUSD sold into the TUSD bids");
                // the ask at the reference would route at a loss (the books' spread) and earns nothing netted: it rests
                assert_eq!(amount_in(&r, cid(1)), 0, "{tag}");
                assert!(profit(&r) > 0, "{tag}");
            }
        }
    }
}

/// When the surplus is large enough for a KAS bid's minimum fill, the netting sells it and both pair orders fill.
#[test]
fn a_netting_surplus_a_kas_bid_can_take_still_nets() {
    for a in [TBTC, TETH] {
        // 20 USD crossed by 5 %: 1 TUSD of surplus (23.8 KAS) clears the 10 KAS minimum fill of a TUSD bid
        let n = a.of(20);
        let mut v = vec![pair8(a, 1, 1, true, n, a.rate(), 0), pair8(a, 2, 4, false, n, bps(a.rate(), 500), 0)];
        v.extend(books(a));
        let r = run_pair(v, &cfg());
        assert_eq!((amount_in(&r, cid(1)), amount_in(&r, cid(2))), (n, n), "{}", a.name);
        assert!(sum_in(&r, 200..206) > 0, "{}: the surplus sold into a TUSD bid", a.name);
    }
}

/// A single pair order crossing the route (selling A below what the two KAS books pay for it, or buying above what they
/// charge) is routed.
#[test]
fn a_pair_order_crossing_the_kas_books_is_routed() {
    for a in [TBTC, TETH] {
        let n = a.of(20);
        let mut v = vec![pair8(a, 1, 1, true, n, bps(a.rate(), -30), 0)];
        v.extend(books(a));
        let r = run_pair(v, &cfg());
        assert_eq!(amount_in(&r, cid(1)), n, "{}: the pair ask", a.name);
        let mut v = vec![pair8(a, 2, 4, false, n, bps(a.rate(), 30), 0)];
        v.extend(books(a));
        let r = run_pair(v, &cfg());
        assert_eq!(amount_in(&r, cid(2)), n, "{}: the pair bid", a.name);
    }
}

/// Plain crossings of one KAS book: matched once the spread covers the fee (~0.021 KAS for 1 x 1 at the floor rate), not
/// below. 5 USD (119 KAS) crossed by 20 bps earns 0.24 KAS; by 1 bp 0.012 KAS, under the fee.
#[test]
fn plain_crossings_are_matched_when_the_spread_pays_the_fee() {
    for (token, p, n) in [(TOKEN_B, KUSD, 5 * S8), (TOKEN, TETH.kas(), TETH.of(5)), (TOKEN, TBTC.kas(), TBTC.of(5))] {
        let r = run_pair(vec![kas_ask(10, token, p, n), kas_bid(11, token, bps(p, 20), n)], &cfg());
        assert_eq!((amount_in(&r, cid(10)), amount_in(&r, cid(11))), (n, n));
        let r = run_pair(vec![kas_ask(10, token, p, n), kas_bid(11, token, bps(p, 1), n)], &cfg());
        assert!(r.prepared.is_empty(), "1 bp of 119 KAS does not pay the fee");
    }
}

/// Untipped crossed pair orders with no KAS book at all: the crossing is paid in TUSD, which goes to the pair ask (the
/// reference planner holds no inventory), so the matcher earns no KAS and builds nothing; a KAS tip pays the netting.
#[test]
fn untipped_pair_orders_without_kas_books_earn_the_matcher_nothing() {
    for a in [TBTC, TETH] {
        let n = a.of(5);
        for b in [0, 20, 100] {
            let r = run_pair(vec![pair8(a, 1, 1, true, n, a.rate(), 0), pair8(a, 2, 4, false, n, bps(a.rate(), b), 0)], &cfg());
            assert!(r.prepared.is_empty(), "{} +{b} bps untipped", a.name);
            // a tip of 0.05 KAS on the order's 5 USD (sompi per whole A)
            let tip = 5_000_000 * S8 / n;
            let r = run_pair(vec![pair8(a, 1, 1, true, n, a.rate(), tip), pair8(a, 2, 4, false, n, bps(a.rate(), b), 0)], &cfg());
            assert_eq!((amount_in(&r, cid(1)), amount_in(&r, cid(2))), (n, n), "{} +{b} bps tipped", a.name);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// Surplus inventory (owner decision 2026-10-06, `PlannerConfig::inventory`): an untipped crossed pair match with no KAS bid
// able to take its surplus earns no KAS. Under the opt-in policy the operator takes the surplus into its own key (the pair
// ask keeps exactly its floor: `KobPair.sil`, `tOut >= ceil`) and counts its policy value as income.

use kob_executor::matcher::engine::{tick, EngineConfig, Prepared};
use kob_executor::matcher::family::Families;
use kob_executor::matcher::planner::{InventoryPolicy, InventoryToken, Kept, UnitPrice};
use kob_protocol::tx::SigPlan;

fn policy(tokens: Vec<InventoryToken>) -> EngineConfig {
    let mut c = cfg();
    c.planner.inventory = InventoryPolicy { accept_surplus_tokens: true, tokens, ..InventoryPolicy::default() };
    c
}

fn tusd(min_amount: Option<i64>, ref_price: Option<UnitPrice>) -> InventoryToken {
    InventoryToken { token: TOKEN_B, ref_price, min_amount }
}

/// A tick without `check_pair`'s no-inventory rule: every transaction engine-validated, no anomaly, and profitable once the
/// kept inventory's policy value is counted.
fn run_keep(orders: Vec<ListedOrder>, cfg: &EngineConfig) -> TickReport {
    let b = book(orders);
    let inp = input(&b);
    let r = tick(&inp, cfg, &Families::default(), &signer());
    assert!(r.anomalies.is_empty(), "anomalies: {:?}", r.anomalies);
    assert_eq!(r.slack_retries, 0);
    for p in &r.prepared {
        assert!(p.validation.is_some(), "unvalidated transaction");
        assert!(p.accounting.profit.saturating_add(p.plan.kept_value()) >= 1, "unprofitable: {:?}", p.accounting);
    }
    r
}

/// Per token of a built transaction: Σ base units of its token inputs, and its token outputs as (owner, amount).
fn token_flows(p: &Prepared, token: [u8; 32]) -> (i64, Vec<([u8; 32], i64)>) {
    let built = &p.lowered.built;
    let mut ins = 0;
    let mut outs = vec![];
    for (k, plan) in built.plans.iter().enumerate() {
        if built.tx.inputs[k].utxo.covenant_id != Some(token) {
            continue;
        }
        match plan {
            SigPlan::TokenLeader { state, next_states, .. } => {
                ins += state.amount;
                outs = next_states.iter().map(|s| (s.owner, s.amount)).collect();
            }
            SigPlan::TokenDelegator { state, .. } => ins += state.amount,
            _ => {}
        }
    }
    (ins, outs)
}

/// The crossed TBTC/TUSD pair (20 USD, the bid 100 bps above the ask, untipped), its fill n, the B the ask needs (its ceil)
/// and the surplus: the B the bid releases (its floor) less that.
fn crossed() -> (Vec<ListedOrder>, i64, i64, i64) {
    let n = TBTC.of(20);
    let (p, q) = (TBTC.rate(), bps(TBTC.rate(), 100));
    let need = ((n as i128 * p as i128 + S8 as i128 - 1) / S8 as i128) as i64;
    let pays = (n as i128 * q as i128 / S8 as i128) as i64;
    (vec![pair8(TBTC, 1, 1, true, n, p, 0), pair8(TBTC, 2, 4, false, n, q, 0)], n, need, pays - need)
}

/// (a) The zero-tip crossed direct match with no TBTC book: off, nothing (the surplus would go to the ask and the matcher
/// earns no KAS); on, with a fillable TUSD bid whose minimum fill (10 KAS, ~0.42 TUSD) is above the 0.2 TUSD surplus, the
/// batch nets both orders, the ask gets exactly its floor, the operator's key the surplus, every token amount conserved.
#[test]
fn a_kept_surplus_pays_a_zero_tip_crossed_match() {
    let (pairs, n, need, surplus) = crossed();
    assert!(surplus > 0 && surplus < 42_000_000, "0.2 TUSD: below a wallet bid's minimum fill");
    let mut v = pairs.clone();
    v.push(kas_bid(200, TOKEN_B, bps(KUSD, -2), 6 * S8));
    // policy off (the default), with and without the TUSD bid: no batch
    assert!(run_keep(pairs.clone(), &cfg()).prepared.is_empty(), "off, no KAS book");
    assert!(run_keep(v.clone(), &cfg()).prepared.is_empty(), "off, with the TUSD bid");
    // policy on, small surpluses accepted (minAmount 0: a later sale of the whole holding clears them)
    let r = run_keep(v, &policy(vec![tusd(Some(0), None)]));
    assert_eq!(r.prepared.len(), 1, "skipped: {:?}", r.skipped);
    let p = &r.prepared[0];
    assert_eq!((amount_in(&r, cid(1)), amount_in(&r, cid(2))), (n, n), "both pair orders netted whole");
    assert_eq!(amount_in(&r, cid(200)), 0, "the TUSD bid only values the surplus");
    // the value: what the bid pays for 0.2 TUSD, at the 80 % haircut
    let at_bid = surplus as i128 * bps(KUSD, -2) as i128 / S8 as i128;
    assert_eq!(p.plan.kept, vec![Kept { token: TOKEN_B, amount: surplus, value: (at_bid * 8_000 / 10_000) as i64 }]);
    assert_eq!(batch_request(p).keep_surplus, vec![TOKEN_B]);
    // TUSD: the ask's delivery is exactly its ceil, the operator's output the surplus, nothing created or lost
    let (b_in, b_out) = token_flows(p, TOKEN_B);
    assert_eq!(b_out.iter().filter(|(o, _)| *o == pk(1)).map(|x| x.1).collect::<Vec<_>>(), vec![need], "the ask's floor");
    assert_eq!(b_out.iter().filter(|(o, _)| *o == pk(MATCHER)).map(|x| x.1).collect::<Vec<_>>(), vec![surplus], "the taker");
    assert_eq!(b_in, b_out.iter().map(|x| x.1).sum::<i64>(), "TUSD conserved");
    // TBTC: the bid receives exactly n, conserved, none to the operator
    let (a_in, a_out) = token_flows(p, TOKEN);
    assert_eq!(a_out.iter().filter(|(o, _)| *o == pk(4)).map(|x| x.1).collect::<Vec<_>>(), vec![n]);
    assert!(a_out.iter().all(|(o, _)| *o != pk(MATCHER)));
    assert_eq!(a_in, a_out.iter().map(|x| x.1).sum::<i64>(), "TBTC conserved");
    // the KAS accounting pays the fee (the operator output's carrier is its own KAS); the inventory pays for it
    assert!(p.accounting.profit < 0 && p.accounting.profit + p.plan.kept_value() > 0, "{:?}", p.accounting);
    // the owner's reference price values it with no KAS book at all
    let r = run_keep(pairs, &policy(vec![tusd(None, Some(UnitPrice { sompi: 2_300_000_000, per: S8 as u64 }))]));
    assert_eq!(r.prepared.len(), 1, "skipped: {:?}", r.skipped);
    assert_eq!(r.prepared[0].plan.kept[0].amount, surplus);
}

/// (b) A token the allowlist does not name (or the switch off) is never kept: its surplus goes to the ask as before.
#[test]
fn an_unlisted_token_is_never_kept() {
    let (mut v, ..) = crossed();
    v.push(kas_bid(200, TOKEN_B, bps(KUSD, -2), 6 * S8));
    let only_tbtc = InventoryToken { token: TOKEN, ref_price: None, min_amount: Some(0) };
    assert!(run_keep(v.clone(), &policy(vec![only_tbtc])).prepared.is_empty(), "TUSD not listed");
    let mut off = policy(vec![tusd(Some(0), None)]);
    off.planner.inventory.accept_surplus_tokens = false;
    assert!(run_keep(v, &off).prepared.is_empty(), "listed but switched off");
}

/// (c) The valuation counts only what fillable bids take: by default never a surplus below the bids' minimum fill
/// (unsellable dust), never more depth than the bids hold.
#[test]
fn the_valuation_never_uses_a_bid_that_cannot_take_it() {
    let (pairs, ..) = crossed();
    let with = |b: ListedOrder| {
        let mut v = pairs.clone();
        v.push(b);
        v
    };
    // default minAmount: 0.2 TUSD is below the bid's 10 KAS minimum fill, so no sale of it alone fills the bid
    assert!(run_keep(with(kas_bid(200, TOKEN_B, bps(KUSD, -2), 6 * S8)), &policy(vec![tusd(None, None)])).prepared.is_empty());
    // a bid holding 1/1000 of the surplus values only that part: below the fee
    assert!(run_keep(with(kas_bid(200, TOKEN_B, bps(KUSD, -2), 20_000)), &policy(vec![tusd(Some(0), None)])).prepared.is_empty());
}

/// The same-transaction sale stays preferred: a surplus a KAS bid can take (1 TUSD clears its minimum fill) is sold there and
/// nothing is kept.
#[test]
fn a_surplus_a_kas_bid_takes_is_sold_not_kept() {
    let n = TBTC.of(20);
    let v = vec![
        pair8(TBTC, 1, 1, true, n, TBTC.rate(), 0),
        pair8(TBTC, 2, 4, false, n, bps(TBTC.rate(), 500), 0),
        kas_bid(200, TOKEN_B, bps(KUSD, -2), 6 * S8),
    ];
    let r = run_keep(v, &policy(vec![tusd(Some(0), None)]));
    assert_eq!(r.prepared.len(), 1, "skipped: {:?}", r.skipped);
    assert!(amount_in(&r, cid(200)) > 0, "sold into the TUSD bid");
    assert!(r.prepared[0].plan.kept.is_empty(), "nothing kept: {:?}", r.prepared[0].plan.kept);
}

// ---------------------------------------------------------------------------------------------------------------------------
// The carrier of the kept output (owner 2026-10-06: "small enough that there is no penalty"). `InventoryPolicy::keep_carrier`
// (default `KEEP_CARRIER`, 2 KAS) is the KAS locked on the operator's inventory output until the owner sells it; the order
// outputs keep their own carriers.

use kob_executor::matcher::planner::{KEEP_CARRIER, KEEP_CARRIER_MIN};
use kob_executor::testkit::TOKEN_COV_KRON;
use kob_protocol::artifacts::TemplateId;

/// The one built transaction of `orders` under `cfg` with the kept output's carrier set to `carrier`.
fn keep_at(orders: &[ListedOrder], cfg: &EngineConfig, carrier: u64) -> Prepared {
    let mut c = cfg.clone();
    c.planner.inventory.keep_carrier = carrier;
    let r = run_keep(orders.to_vec(), &c);
    assert_eq!(r.prepared.len(), 1, "carrier {carrier}: skipped {:?}", r.skipped);
    r.prepared[0].clone()
}

/// The two builds differ, output by output, only in the operator's change and in the kept token output of `token` (10 KAS
/// against `KEEP_CARRIER`): every order output is the same.
fn only_the_kept_carrier_differs(def: &Prepared, ten: &Prepared, token: [u8; 32], tag: &str) {
    let (a, b) = (&def.lowered.built.tx.outputs, &ten.lowered.built.tx.outputs);
    assert_eq!(a.len(), b.len(), "{tag}");
    let change = def.lowered.built.fee.change_output.map(|i| i as usize);
    assert_eq!(change, ten.lowered.built.fee.change_output.map(|i| i as usize), "{tag}");
    let mut kept = 0;
    for (k, (x, y)) in a.iter().zip(b).enumerate() {
        if Some(k) == change {
            continue;
        }
        assert_eq!((&x.script_public_key, &x.covenant), (&y.script_public_key, &y.covenant), "{tag}: output {k}");
        if x.value != y.value {
            assert_eq!((x.value, y.value), (KEEP_CARRIER, 10 * KAS), "{tag}: output {k} is an order output");
            assert_eq!(x.covenant.as_ref().map(|c| c.covenant_id), Some(token), "{tag}: output {k}");
            kept += 1;
        }
    }
    assert_eq!(kept, 1, "{tag}: exactly the kept output's carrier changes");
}

/// The default keep carrier adds no fee: the zero-tip TBTC/TUSD keep-surplus batch, and the smallest keep batch of every
/// family pair (a 1 x 1 netting kept by `refPrice`), have the same size, fee mass, fee and storage-inclusive priority mass at
/// `KEEP_CARRIER` as at 10 KAS; the order outputs are identical, the operator's KAS profit is the same (the carrier is its
/// own KAS: what the smaller carrier does not lock returns in the change), and every token is conserved. Below it the
/// storage mass (`4 × 10^12 / carrier`, plurality 2) overtakes the fee mass of the smallest batch (KRON / KRON) at 1.5 KAS.
#[test]
fn the_kept_output_carrier_adds_no_fee() {
    const TEN: u64 = 10 * KAS;
    let (pairs, ..) = crossed();
    let mut tbtc = pairs.clone();
    tbtc.push(kas_bid(200, TOKEN_B, bps(KUSD, -2), 6 * S8));
    let mut cases = vec![("TBTC/TUSD 8/8, zero tip, bid-valued".to_string(), tbtc, policy(vec![tusd(Some(0), None)]), TOKEN_B)];
    for (pa, pb) in [(T3, T3), (T8, T8), (TemplateId::KronToken2433, TemplateId::KronToken2433), (T3, TemplateId::KronToken2433)] {
        let n = 3 * WHOLE;
        let v = vec![
            l_pair(1, pa, pb, pask(1, pa, pb, n, RATE, 0), 1_000),
            l_pair(2, pa, pb, pbid(4, pa, pb, n, RATE * 11 / 10, 0), 1_000),
        ];
        let tb = token_b(pa, pb);
        let pol = policy(vec![InventoryToken { token: tb, ref_price: Some(UnitPrice { sompi: 10 * KAS, per: 1 }), min_amount: None }]);
        cases.push((format!("{} / {} netting 1 x 1", pa.name(), pb.name()), v, pol, tb));
    }
    let mut smallest_fee_mass = u64::MAX;
    for (tag, orders, cfg, token) in &cases {
        let ten = keep_at(orders, cfg, TEN);
        let def = keep_at(orders, cfg, KEEP_CARRIER);
        let (m10, m) = (&ten.lowered.built.fee.mass, &def.lowered.built.fee.mass);
        println!(
            "{tag}: 10 KAS: bytes {} fee mass {} storage {} fee {} | {} sompi: storage {} fee {} priority mass {}",
            m10.size, m10.fee_mass, m10.storage, ten.accounting.fee, KEEP_CARRIER, m.storage, def.accounting.fee, m.priority_mass
        );
        assert_eq!((m.size, m.fee_mass, def.accounting.fee), (m10.size, m10.fee_mass, ten.accounting.fee), "{tag}: the fee");
        assert_eq!(m.priority_mass, m10.priority_mass, "{tag}: the storage mass stays below the fee mass");
        assert!(m.storage < m.fee_mass, "{tag}");
        only_the_kept_carrier_differs(&def, &ten, *token, tag);
        assert_eq!(def.accounting.profit, ten.accounting.profit, "{tag}: the carrier is the operator's own KAS");
        assert_eq!(def.lowered.operator_token_kas_out, KEEP_CARRIER, "{tag}: the accounting counts the kept output's carrier");
        assert_eq!(def.accounting.change, ten.accounting.change + (TEN - KEEP_CARRIER), "{tag}: the rest returns in the change");
        for t in [TOKEN, TOKEN_B, TOKEN_COV_KRON, TOKEN_B_KRON] {
            let (i, o) = token_flows(&def, t);
            assert_eq!(i, o.iter().map(|x| x.1).sum::<i64>(), "{tag}: conserved");
        }
        smallest_fee_mass = smallest_fee_mass.min(m.fee_mass);
    }
    // the bound is tight where it matters: the smallest batch (KRON / KRON) pays a higher priority fee at 1.5 KAS
    let (_, kron, cfg, _) = &cases[3];
    let p = keep_at(kron, cfg, 150_000_000);
    let m = &p.lowered.built.fee.mass;
    assert_eq!(m.fee_mass, smallest_fee_mass, "the KRON / KRON netting is the smallest keep batch");
    assert!(m.storage > m.fee_mass, "1.5 KAS: storage {} above the fee mass {}", m.storage, m.fee_mass);
    // and the relay fee does not price storage at all: the same fee down to the policy's least carrier
    let low = keep_at(kron, cfg, KEEP_CARRIER_MIN);
    assert_eq!(low.accounting.fee, keep_at(kron, cfg, TEN).accounting.fee);
}

/// The policy refuses a keep carrier below the largest token-program floor, and KaspaCom's 0.5 KAS floor is met at it.
#[test]
fn the_keep_carrier_has_a_floor() {
    let mut p = policy(vec![]).planner.inventory;
    p.keep_carrier = KEEP_CARRIER_MIN - 1;
    assert!(p.check().is_err());
    p.keep_carrier = KEEP_CARRIER_MIN;
    p.check().expect("the floor itself");
    let k = TemplateId::Kcc20KaspaCom025;
    assert_eq!(k.min_token_output(), Some(KEEP_CARRIER_MIN));
    let n = 3 * WHOLE;
    let v = vec![l_pair(1, k, k, pask(1, k, k, n, RATE, 0), 1_000), l_pair(2, k, k, pbid(4, k, k, n, RATE * 11 / 10, 0), 1_000)];
    let pol = policy(vec![InventoryToken {
        token: token_b(k, k),
        ref_price: Some(UnitPrice { sompi: 10 * KAS, per: 1 }),
        min_amount: None,
    }]);
    let at = keep_at(&v, &pol, KEEP_CARRIER_MIN);
    assert!(at.validation.is_some());
}
