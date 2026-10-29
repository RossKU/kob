//! Audit 2026-10-30 V1 regression: one bare placement of another strict program must not lock an open-list token out as
//! `program_ambiguous`. A program the chain proves (the token's holdings) wins, the conflicting placements are ignored and not
//! listed; only when nothing proves a program do several claims stay ambiguous.

mod common;

use std::sync::Arc;

use common::*;
use kob_executor::hex::{self, Hash32};
use kob_executor::indexer::processor::Processor;
use kob_executor::indexer::reads;
use kob_executor::testkit::*;
use kob_executor::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;

const KR: TemplateId = TemplateId::KaspaRocketToken11458;

fn open_list() -> TokenAllowlist {
    let strict = |id: TemplateId| {
        let p = kob_protocol::profile::of(id).unwrap();
        (
            kob_executor::hex::Hash32::parse(&p.template_hash).unwrap(),
            StrictTemplate { id: p.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] },
        )
    };
    TokenAllowlist::from_entries([]).with_strict_templates([strict(T3), strict(KR)])
}

fn ctx() -> (Ctx, Arc<TokenAllowlist>) {
    let tokens = Arc::new(open_list());
    let rules = ListingRules { max_expiry_span_daa: 1 << 40, min_order_value_sompi: 0, ..ListingRules::default() };
    let proc = Processor { tokens: tokens.clone(), rules };
    (Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc)), tokens)
}

fn bid_on(id: TemplateId, scale: i64, price: i64) -> BidState {
    let (h, p, s) = tok_fields(id);
    BidState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(id), scale, price, ..bid(MAKER_B, price) }
}

fn token_row(c: &Ctx, tokens: &TokenAllowlist) -> Vec<reads::TokenView> {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let g = c.hs.ingest.lock().unwrap();
    reads::token_list(g.conn(), tokens).unwrap().into_iter().filter(|t| t.covenant_id == hex(&TOKEN_COV)).collect()
}

async fn evil_bid(c: &Ctx) -> Hash32 {
    let evil = bid_on(KR, SCALE, 250_000_000);
    let escrow = evil.escrow(WHOLE, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(evil), escrow, MAKER_B, 0);
    c.push(&[&tx]).await;
    c.w.cov(&tx, 0)
}

async fn honest_ask(c: &Ctx) -> Hash32 {
    let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    c.w.cov(&create, 0)
}

#[tokio::test]
async fn a_program_the_holdings_prove_wins_over_a_bare_placement_that_comes_after() {
    let (c, tokens) = ctx();
    let ask_cov = honest_ask(&c).await;
    let t3_hash = tok_fields(T3).0;
    let evil = evil_bid(&c).await;
    let ev = view(&c, &evil);
    assert!(!ev.listed, "the conflicting placement is not listed");
    assert_eq!(ev.unlisted_reason.as_deref(), Some("token_program_conflict"));
    assert!(view(&c, &ask_cov).listed, "the honest ask stays listed");
    let rows = token_row(&c, &tokens);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].program_ambiguous, rows[0].template_hash.as_deref()), (None, Some(hex::encode(&t3_hash).as_str())));
}

#[tokio::test]
async fn a_program_the_holdings_prove_wins_over_a_bare_placement_that_came_first() {
    let (c, tokens) = ctx();
    let _ = evil_bid(&c).await;
    let before = token_row(&c, &tokens);
    assert_eq!(before.len(), 1, "one claim, nothing proves another");
    // the honest ask creates the token's holdings: the chain proves the program
    let _ = honest_ask(&c).await;
    let rows = token_row(&c, &tokens);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].program_ambiguous, rows[0].template_hash.as_deref()), (None, Some(hex::encode(&tok_fields(T3).0).as_str())));
}

#[tokio::test]
async fn several_claims_nothing_proves_stay_ambiguous() {
    let (c, tokens) = ctx();
    let honest = bid_on(T3, SCALE, 250_000_000);
    let escrow = honest.escrow(WHOLE, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(honest), escrow, MAKER_A, 0);
    c.push(&[&tx]).await;
    let _ = evil_bid(&c).await;
    let rows = token_row(&c, &tokens);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].program_ambiguous, rows[0].template_hash.clone()), (Some(true), None));
    assert_eq!(rows[0].candidate_templates.len(), 2);
}
