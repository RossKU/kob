//! Audit 2026-10-30 E1, listing: a stop a keeper could arm only into a dust continuation is not listed; orders no keeper updates are
//! not affected by their tip.

mod common;

use common::*;
use kob_executor::testkit::*;
use kob_protocol::state::*;

async fn listed(c: &Ctx, co: CondAskState) -> (bool, Option<String>) {
    let create = c.w.create_tx(AnyState::KobCondAsk(co), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let v = view(c, &c.w.cov(&create, 0));
    (v.listed, v.unlisted_reason)
}

#[tokio::test]
async fn a_stop_whose_tip_leaves_a_dust_continuation_is_not_listed() {
    let bound = kob_protocol::tx::COVENANT_OUTPUT_MIN as i64;
    // honest: the default tip leaves the order's carrier almost whole
    let (ok, why) = listed(&Ctx::new(), cond_ask(MAKER_A)).await;
    assert!(ok, "{why:?}");
    // hostile: the tip leaves 0.01 KAS
    let hostile = CondAskState { keeper_tip: CARRIER as i64 - 1_000_000, ..cond_ask(MAKER_A) };
    assert_eq!(listed(&Ctx::new(), hostile).await, (false, Some("keeper_tip_leaves_dust".into())));
    // exactly the bound is fine
    let edge = CondAskState { keeper_tip: CARRIER as i64 - bound, ..cond_ask(MAKER_A) };
    assert!(listed(&Ctx::new(), edge).await.0);
    // a take-profit without a stop is never updated: its tip is dead
    let tp_only = CondAskState { stop_price: 0, keeper_tip: CARRIER as i64 - 1, ..cond_ask(MAKER_A) };
    let (ok, why) = listed(&Ctx::new(), tp_only).await;
    assert!(ok, "{why:?}");
}
