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
