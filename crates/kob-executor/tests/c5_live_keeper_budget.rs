//! C5 liveness audit, keepers: a build that always fails is retried every tick and eats the attempt budget.
//!
//! `keepers::tick` sorts the due orders by (kind, refund time, id), builds at most `max_attempts` (128) jobs per tick and
//! STOPS the loop at the budget (`keepers/mod.rs`: `if attempts >= cfg.max_attempts { ...; break; }`). A job that fails to
//! build (`Err(e) => report.skipped.push(..)`) is counted as an attempt but, unlike an unprofitable one
//! (`known_unprofitable`), is never remembered, and the matcher's `frozen_until` set is not in the keepers' `excluded`
//! either. An order whose refund can never be built therefore costs one attempt EVERY tick, and 128 of them in front of the
//! queue (earliest refund time first, kills before refunds) starve every later refund and kill of the operator.
//!
//! The cheapest such order: a `refundTip` larger than the order's own value. The numeric gate allows a tip up to 2^60
//! (`sanity.rs`, `c.range("refundTip", .., 0, MONEY_MAX)`), the listing rules do not look at it, the fee-floor pre-check only
//! looks at `tip - floor >= minProfit` (a huge tip passes), and `build_refund_order` then fails with
//! `refund payout would be negative`. A frozen custody (the engine rejects the refund) has the same effect.

#[path = "matcher_common/mod.rs"]
mod common;

use std::collections::BTreeSet;

use common::*;
use kob_executor::keepers::{tick, JobKind, KeeperConfig, KeeperInput, KeeperReport};
use kob_executor::matcher::book::{Clock, ListedOrder};
use kob_protocol::state::*;

fn keep(orders: Vec<ListedOrder>, daa: u64, cfg: &KeeperConfig) -> KeeperReport {
    let inp = KeeperInput {
        orders,
        clock: Clock { daa, utc: UTC },
        funding: vec![funding(20 * KAS)],
        excluded: BTreeSet::new(),
        excluded_outpoints: BTreeSet::new(),
        known_unprofitable: BTreeSet::new(),
    };
    tick(&inp, cfg, &signer())
}

/// An IOC ask whose refund tip exceeds its whole carrier: due at `utxo_daa + 600`, never buildable.
fn hostile_kill(n: u32, utxo_daa: u64) -> ListedOrder {
    let mut a = ask(1, P250, 5 * WHOLE, T3);
    a.tif = TIF_IOC;
    a.expiry_daa = NO_EXPIRY;
    a.refund_tip = 100 * KAS as i64; // the order UTXO holds CARRIER = 10 KAS
    listed(cid(n), AnyState::KobAsk(a), CARRIER, utxo_daa)
}

/// A normal IOC ask with the program's default tip, due later than every hostile one.
fn honest_kill(n: u32, utxo_daa: u64) -> ListedOrder {
    let mut a = ask(2, P250, 5 * WHOLE, T3);
    a.tif = TIF_IOC;
    a.expiry_daa = NO_EXPIRY;
    listed(cid(n), AnyState::KobAsk(a), CARRIER, utxo_daa)
}

#[test]
fn one_unbuildable_order_does_not_stop_the_honest_kill() {
    // control: a single hostile order costs one attempt and the honest order is still killed (passes today)
    let cfg = KeeperConfig::default();
    let r = keep(vec![hostile_kill(1, 1_000), honest_kill(9_999, 2_000)], 3_000, &cfg);
    assert!(r.jobs.iter().any(|j| j.kind == JobKind::Kill && j.order == cid(9_999)), "skipped: {:?}", r.skipped);
}

#[test]
fn a_crowd_of_unbuildable_refunds_cannot_starve_the_keeper() {
    // C5: expected to fail until failed builds are remembered per (order, outpoint) like `known_unprofitable` (or the attempt
    // budget is spent fairly, e.g. a rotating start), and possibly-frozen orders get the matcher's `frozen_until` backoff.
    let cfg = KeeperConfig::default();
    assert_eq!(cfg.max_attempts, 128);
    let mut orders: Vec<ListedOrder> = (1..=cfg.max_attempts as u32 + 2).map(|n| hostile_kill(n, 1_000)).collect();
    orders.push(honest_kill(9_999, 2_000));
    // every order is due at 3_000 (hostile: 1_600, honest: 2_600); the hostile ones sort first (same kind, earlier due)
    let r = keep(orders, 3_000, &cfg);
    assert!(
        r.jobs.iter().any(|j| j.kind == JobKind::Kill && j.order == cid(9_999)),
        "the honest kill was never reached: {} jobs, {} skipped, last skip: {:?}",
        r.jobs.len(),
        r.skipped.len(),
        r.skipped.last()
    );
}
