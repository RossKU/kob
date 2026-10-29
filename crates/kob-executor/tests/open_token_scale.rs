//! Audit 2026-10-29 F3: what `GET /v1/tokens` says about an open-list token (no registry entry) is not whatever the first order
//! placements claim. A KaspaRocket-program token has the program convention of 3 decimals (scale 1000): an order at another scale is
//! not listed, so it can not become the token's `market_scale` beside decimals the clients refuse to trade against; and placements that
//! name several programs for one covenant id replace nothing: the token is listed as ambiguous, with no program.
//!
//! Run: cargo test -p kob-executor --test open_token_scale

mod common;

use std::sync::Arc;

use common::*;
use kob_executor::indexer::processor::Processor;
use kob_executor::indexer::reads;
use kob_executor::testkit::*;
use kob_executor::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::state::*;

const KR: TemplateId = TemplateId::KaspaRocketToken11458;
const PM: TemplateId = TemplateId::Kcc20PublicMint;

fn open_list() -> TokenAllowlist {
    let strict = |id: TemplateId| {
        let p = kob_protocol::profile::of(id).unwrap();
        (
            kob_executor::hex::Hash32::parse(&p.template_hash).unwrap(),
            StrictTemplate { id: p.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] },
        )
    };
    TokenAllowlist::from_entries([]).with_strict_templates([strict(KR), strict(PM)])
}

fn ctx() -> (Ctx, Arc<TokenAllowlist>) {
    let tokens = Arc::new(open_list());
    // production-like rules: the 1 KAS default minimum order value
    let rules = ListingRules { max_expiry_span_daa: 1 << 40, ..ListingRules::default() };
    let proc = Processor { tokens: tokens.clone(), rules };
    (Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc)), tokens)
}

fn bid_on(id: TemplateId, scale: i64, price: i64) -> BidState {
    let (h, p, s) = tok_fields(id);
    BidState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(id), scale, price, ..bid(MAKER_B, price) }
}

fn token_row(c: &Ctx, tokens: &TokenAllowlist) -> Vec<reads::TokenView> {
    let g = c.hs.ingest.lock().unwrap();
    reads::token_list(g.conn(), tokens).unwrap().into_iter().filter(|t| t.covenant_id == hex(&TOKEN_COV)).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[tokio::test]
async fn an_order_at_a_wrong_scale_is_not_listed_and_never_becomes_the_market_scale() {
    let (c, tokens) = ctx();
    // the attacker's 20 KAS bid at scale 1 (price per base unit), on a KaspaRocket-program token; the convention is 1000
    let evil = bid_on(KR, 1, 250_000);
    let escrow = evil.escrow(20_000, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(evil), escrow, MAKER_B, 0);
    c.push(&[&tx]).await;
    let v = view(&c, &c.w.cov(&tx, 0));
    assert!(!v.listed, "an order at a scale other than the program's convention is not listed for an open token");
    assert_eq!(v.unlisted_reason.as_deref(), Some("non_standard_scale"));

    // an honest order at the convention's scale afterwards is listed
    let honest = bid_on(KR, 1000, 250_000_000);
    let escrow = honest.escrow(20 * WHOLE, 2).unwrap() as u64;
    let tx2 = c.w.create_tx(AnyState::KobBid(BidState { maker: pk(MAKER_A), ..honest }), escrow, MAKER_A, 0);
    c.push(&[&tx2]).await;
    assert!(view(&c, &c.w.cov(&tx2, 0)).listed);

    let rows = token_row(&c, &tokens);
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!((r.decimals, r.decimals_source), (Some(3), Some("program")));
    assert_eq!(r.market_scale, None, "a token whose decimals are resolved serves no market scale beside them");
    assert_eq!(r.program_ambiguous, None);
}

#[tokio::test]
async fn a_token_without_decimals_keeps_the_scale_of_its_first_order() {
    let (c, tokens) = ctx();
    // the public-mint program has no default decimals: its first order sets the market's scale (as before)
    let first = bid_on(PM, 1_000_000, 250_000_000);
    let escrow = first.escrow(20 * 1_000_000, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(first), escrow, MAKER_A, 0);
    c.push(&[&tx]).await;
    assert!(view(&c, &c.w.cov(&tx, 0)).listed);
    let rows = token_row(&c, &tokens);
    assert_eq!((rows[0].decimals, rows[0].market_scale), (None, Some(1_000_000)));
}

#[tokio::test]
async fn a_placement_naming_another_strict_program_makes_the_token_ambiguous_instead_of_replacing_its_program() {
    let (c, tokens) = ctx();
    // the honest first order of the token: the public-mint program (decimals unknown)
    let honest = bid_on(PM, 100_000_000, 250_000_000);
    let escrow = honest.escrow(20 * 100_000_000, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(honest), escrow, MAKER_A, 0);
    c.push(&[&tx]).await;
    let before = token_row(&c, &tokens);
    let pm_hash = before[0].template_hash.clone().unwrap();
    assert_eq!(before[0].program_ambiguous, None);

    // anyone's order that pins the same covenant id with the KaspaRocket program (never fillable: nothing of the token is of it)
    let evil = bid_on(KR, 1000, 250_000_000);
    let escrow = evil.escrow(20 * WHOLE, 2).unwrap() as u64;
    let tx2 = c.w.create_tx(AnyState::KobBid(evil), escrow, MAKER_B, 0);
    c.push(&[&tx2]).await;
    let after = token_row(&c, &tokens);
    assert_eq!(after.len(), 1, "one row per token");
    let r = &after[0];
    assert_eq!(r.program_ambiguous, Some(true), "two programs are claimed for the covenant id: the indexer names none");
    assert_eq!((r.template_hash.clone(), r.template_id.clone(), r.family), (None, None, None));
    assert!(r.candidate_templates.contains(&pm_hash), "{:?}", r.candidate_templates);
    assert_eq!(r.candidate_templates.len(), 2);
    assert_eq!((r.decimals, r.decimals_source), (None, None), "no program, no program convention");
}
