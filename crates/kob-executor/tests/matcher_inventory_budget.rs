//! The surplus-inventory budgets (`tokens[].maxAmount`, `maxFee`) bound a whole tick: each batch of the tick is planned
//! against what the batches before it left, so batches of different pair markets that keep the same token cannot each
//! use the whole budget.

#[path = "matcher_common/mod.rs"]
mod common;

use common::pair::*;
use common::*;
use kob_executor::matcher::book::ListedOrder;
use kob_executor::matcher::engine::{tick, EngineConfig, TickReport};
use kob_executor::matcher::family::Families;
use kob_executor::matcher::planner::{InventoryPolicy, InventoryToken, UnitPrice};
use kob_protocol::defaults::{default_min_fill, default_min_fill_pair};
use kob_protocol::state::*;

const S8: i64 = 100_000_000;
const KUSD: i64 = 2_380_000_000;
const USD: i64 = 100_000;

fn bps(p: i64, b: i64) -> i64 {
    (p as i128 * (10_000 + b) as i128 / 10_000) as i64
}

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

/// A crossed pair of base token `a` against TUSD (TOKEN_B), 20 USD, the bid 100 bps above the ask, untipped: its TUSD
/// surplus.
fn crossed(a: [u8; 32], id0: u32, maker0: u8) -> (Vec<ListedOrder>, i64) {
    let n = 20 * S8 / USD;
    let (p, q) = (USD * S8, bps(USD * S8, 100));
    let mk = |id: u32, maker: u8, ask: bool, price: i64| {
        let mut s = pair_state(maker, ask, T8, T8, n, price, 0, TIF_GTC);
        s.s_scale = S8;
        s.t_scale = S8;
        s.min_fill = default_min_fill_pair(n, Some(USD * KUSD), S8);
        s.custody = if ask { n } else { s.bid_escrow(n, 4).expect("escrow") };
        (s.s_cov_id, s.t_cov_id) = if ask { (a, TOKEN_B) } else { (TOKEN_B, a) };
        l_pair(id, T8, T8, s, 1_000)
    };
    let need = ((n as i128 * p as i128 + S8 as i128 - 1) / S8 as i128) as i64;
    let pays = (n as i128 * q as i128 / S8 as i128) as i64;
    (vec![mk(id0, maker0, true, p), mk(id0 + 1, maker0 + 1, false, q)], pays - need)
}

fn policy(max_amount: Option<i64>, max_fee: Option<u64>) -> EngineConfig {
    let mut c = cfg();
    let rp = UnitPrice { sompi: 2_300_000_000, per: S8 as u64 };
    c.planner.inventory = InventoryPolicy {
        accept_surplus_tokens: true,
        tokens: vec![InventoryToken { token: TOKEN_B, ref_price: Some(rp), min_amount: None, max_amount }],
        max_fee,
        ..InventoryPolicy::default()
    };
    c
}

fn run(orders: Vec<ListedOrder>, cfg: &EngineConfig) -> TickReport {
    let b = book(orders);
    tick(&input(&b), cfg, &Families::default(), &signer())
}

fn kept(r: &TickReport) -> (usize, i64) {
    let n = r.prepared.iter().filter(|p| !p.plan.kept.is_empty()).count();
    let total = r.prepared.iter().flat_map(|p| p.plan.kept.iter()).filter(|k| k.token == TOKEN_B).map(|k| k.amount).sum();
    (n, total)
}

/// Crossed pairs of several pair markets (base tokens TOKEN, 0x5c.., 0x5d..), all with a TUSD surplus, and a byte budget
/// that puts each in a batch of its own.
fn markets() -> (Vec<ListedOrder>, i64, u64) {
    let (o, surplus) = crossed(TOKEN, 10, 10);
    let mut one = vec![kas_bid(900, TOKEN_B, bps(KUSD, -2), 6 * S8)];
    one.extend(o.clone());
    let r = run(one, &policy(None, None));
    assert_eq!(kept(&r), (1, surplus), "one pair keeps its surplus");
    let bytes = r.prepared[0].accounting.bytes;
    let mut v = vec![kas_bid(900, TOKEN_B, bps(KUSD, -2), 6 * S8)];
    v.extend(o);
    for (j, a) in [[0x5c; 32], [0x5d; 32]].into_iter().enumerate() {
        let (o, _) = crossed(a, 20 + 2 * j as u32, 20 + 2 * j as u8);
        v.extend(o);
    }
    (v, surplus, bytes)
}

#[test]
fn the_batches_of_one_tick_share_the_amount_budget() {
    let (v, surplus, bytes) = markets();
    let tight = |max: Option<i64>| {
        let mut c = policy(max, None);
        c.planner.max_tx_bytes = bytes + bytes / 4;
        c
    };
    let r = run(v.clone(), &tight(None));
    let (n, total) = kept(&r);
    assert!(n >= 2 && total > surplus, "control: unbounded, several batches keep more than one surplus ({n}, {total})");
    let r = run(v, &tight(Some(surplus)));
    let (n, total) = kept(&r);
    assert!(total <= surplus, "one tick kept {total} > maxAmount {surplus} over {n} batches");
}

#[test]
fn the_batches_of_one_tick_share_the_fee_budget() {
    let (v, _, bytes) = markets();
    let mut c = policy(None, Some(1));
    c.planner.max_tx_bytes = bytes + bytes / 4;
    let r = run(v, &c);
    assert_eq!(kept(&r).0, 1, "the first batch that keeps a surplus uses the fee budget up");
}
