//! C2-H1: a KCC-20 bid that names an extension commitment no holder of the token carries can never
//! be delivered to, so it must not be listed (a free fake best bid), and a placement that names one
//! must not erase the commitment the token's holdings prove.

mod common;

use std::sync::Arc;

use common::*;
use kob_protocol::state::*;
use kobd::hex::{self, Hash32};
use kobd::indexer::processor::Processor;
use kobd::indexer::reads;
use kobd::testkit::*;
use kobd::tokens::{ListingRules, StrictTemplate, TokenAllowlist};

fn ctx() -> (Ctx, Arc<TokenAllowlist>) {
    let p = kob_protocol::profile::of(T3).unwrap();
    let strict = (
        Hash32::parse(&p.template_hash).unwrap(),
        StrictTemplate { id: p.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] },
    );
    let tokens = Arc::new(TokenAllowlist::from_entries([]).with_strict_templates([strict]));
    let rules = ListingRules { max_expiry_span_daa: 1 << 40, min_order_value_sompi: 0, ..ListingRules::default() };
    let proc = Processor { tokens: tokens.clone(), rules };
    (Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc)), tokens)
}

const FAKE: [u8; 32] = [0x5a; 32];

async fn bid_with(c: &Ctx, ext: [u8; 32], price: i64) -> Hash32 {
    let b = BidState { extension_commitment: ext, ..bid(MAKER_B, price) };
    let escrow = b.escrow(WHOLE, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(b), escrow, MAKER_B, 0);
    c.push(&[&tx]).await;
    c.w.cov(&tx, 0)
}

async fn honest_ask(c: &Ctx) -> Hash32 {
    let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    c.w.cov(&create, 0)
}

fn token_ext(c: &Ctx, tokens: &TokenAllowlist) -> Option<String> {
    let g = c.hs.ingest.lock().unwrap();
    let rows = reads::token_list(g.conn(), tokens).unwrap();
    rows.into_iter().find(|t| t.covenant_id == hex::encode(&TOKEN_COV)).and_then(|t| t.extension_commitment)
}

#[tokio::test]
async fn a_bid_with_an_unproven_extension_is_not_listed_and_does_not_erase_the_tokens() {
    let (c, tokens) = ctx();
    let ask_cov = honest_ask(&c).await;
    assert_eq!(token_ext(&c, &tokens), Some(hex::encode(&EXT)));
    let fake = bid_with(&c, FAKE, 10 * P250).await;
    let v = view(&c, &fake);
    assert!(!v.listed, "the bid no holder can deliver to is not listed");
    assert_eq!(v.unlisted_reason.as_deref(), Some("extension_not_proven"));
    assert_eq!(token_ext(&c, &tokens), Some(hex::encode(&EXT)), "the placement does not erase the proven commitment");
    let honest = bid_with(&c, EXT, P250 / 2).await;
    assert!(view(&c, &honest).listed && view(&c, &ask_cov).listed);
}

#[tokio::test]
async fn a_bid_listed_before_the_first_holding_is_unlisted_by_it() {
    let (c, tokens) = ctx();
    let fake = bid_with(&c, FAKE, 10 * P250).await;
    let honest = bid_with(&c, EXT, P250 / 2).await;
    assert!(view(&c, &fake).listed, "nothing proves a commitment yet");
    let _ = honest_ask(&c).await;
    let v = view(&c, &fake);
    assert_eq!((v.listed, v.unlisted_reason.as_deref()), (false, Some("extension_not_proven")));
    assert!(view(&c, &honest).listed);
    assert_eq!(token_ext(&c, &tokens), Some(hex::encode(&EXT)));
}
