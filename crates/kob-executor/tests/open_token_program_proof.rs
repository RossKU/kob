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
const K2732: TemplateId = TemplateId::KronToken2732;

fn open_list() -> TokenAllowlist {
    let strict = |id: TemplateId| {
        let p = kob_protocol::profile::of(id).unwrap();
        (
            kob_executor::hex::Hash32::parse(&p.template_hash).unwrap(),
            StrictTemplate { id: p.registry_id.clone(), family: kob_protocol::Family::Kcc20, powers: vec![] },
        )
    };
    TokenAllowlist::from_entries([]).with_strict_templates([strict(T3), strict(KR), strict_kron(K3), strict_kron(K2732)])
}

fn strict_kron(id: TemplateId) -> (kob_executor::hex::Hash32, StrictTemplate) {
    (
        kob_executor::hex::Hash32(kob_protocol::artifacts::token_template(id).hash),
        StrictTemplate { id: id.name().into(), family: id.family(), powers: vec![] },
    )
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

// ---------------------------------------------------------------------------------------------- audit 2026-10-30 V2: KRON

fn kron_row(c: &Ctx, tokens: &TokenAllowlist) -> Vec<reads::TokenView> {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let g = c.hs.ingest.lock().unwrap();
    reads::token_list(g.conn(), tokens).unwrap().into_iter().filter(|t| t.covenant_id == hex(&TOKEN_COV_KRON)).collect()
}

async fn honest_kron_ask(c: &Ctx) {
    let tx = c.w.create_tx(kron(AnyState::KobAsk(ask(MAKER_A, P250))), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&tx]).await;
    assert!(view(c, &c.w.cov(&tx, 0)).listed);
}

#[tokio::test]
async fn a_kron_program_the_holdings_prove_is_not_replaced_by_a_bare_bid_of_another_kron_program() {
    let (c, tokens) = ctx();
    honest_kron_ask(&c).await;
    let honest = kron_row(&c, &tokens);
    assert_eq!(honest.len(), 1);
    assert_eq!(honest[0].program_ambiguous, None, "{honest:?}");
    // a bare KRON bid pinning another KRON program (a different slot count) for the same covenant id
    let mut b = bid_on(K3, SCALE, 250_000_000);
    b.token_cov_id = TOKEN_COV_KRON;
    b.extension_commitment = [0; 32];
    let (h, p, s) = tok_fields(K2732);
    (b.token_tpl_hash, b.tpl_prefix_len, b.tpl_suffix_len, b.refund_tip) = (h, p, s, rtip(K2732));
    let escrow = b.escrow(WHOLE, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBidKron(b), escrow, MAKER_B, 0);
    c.push(&[&tx]).await;
    let ev = view(&c, &c.w.cov(&tx, 0));
    assert!(!ev.listed, "the conflicting KRON placement is not listed");
    assert_eq!(ev.unlisted_reason.as_deref(), Some("token_program_conflict"));
    let rows = kron_row(&c, &tokens);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].program_ambiguous, None, "the token stays unambiguous: {rows:?}");
}

#[tokio::test]
async fn a_kcc20_bid_naming_a_kron_covenant_id_is_not_listed_and_does_not_lock_the_token() {
    let (c, tokens) = ctx();
    honest_kron_ask(&c).await;
    let mut b = bid_on(T3, SCALE, 250_000_000);
    b.token_cov_id = TOKEN_COV_KRON;
    let escrow = b.escrow(WHOLE, 2).unwrap() as u64;
    let tx = c.w.create_tx(AnyState::KobBid(b), escrow, MAKER_B, 0);
    c.push(&[&tx]).await;
    let ev = view(&c, &c.w.cov(&tx, 0));
    assert!(!ev.listed, "a KCC-20 placement for a KRON token is refused");
    assert_eq!(ev.unlisted_reason.as_deref(), Some("token_program_conflict"));
    let rows = kron_row(&c, &tokens);
    assert_eq!((rows.len(), rows[0].program_ambiguous), (1, None), "{rows:?}");
}

#[tokio::test]
async fn several_programs_of_one_kron_token_are_all_proven_by_its_holdings() {
    // the slot-count variants are legitimately several: each program a holding shows is proven (the proven set is a skip-scan of the
    // programs, not the first one)
    let (c, _tokens) = ctx();
    honest_kron_ask(&c).await;
    let g = c.hs.ingest.lock().unwrap();
    let conn = g.conn();
    for (i, p) in [K2732, K3].into_iter().enumerate() {
        conn.execute(
            "INSERT OR IGNORE INTO token_holdings (txid, idx, token_cov_id, program, family, owner, owner_kind, amount, value, state, role, created_block, created_daa)              VALUES (?1, 0, ?2, ?3, 2, x'00', 0, 1, 1, x'00', 'owned', 1, 1)",
            rusqlite::params![vec![0xee_u8, i as u8], &TOKEN_COV_KRON[..], p.name()],
        )
        .unwrap();
    }
    let proven = reads::proven_programs(conn, &TOKEN_COV_KRON).unwrap();
    for p in [K2732, K3] {
        assert!(proven.contains(&kob_protocol::artifacts::token_template(p).hash.to_vec()), "{} is proven", p.name());
    }
}

// ---------------------------------------------------------------------------------------------- audit 2026-10-30 V4

#[tokio::test]
async fn a_conflicting_bid_listed_before_the_proof_is_taken_out_of_the_book_and_comes_back_with_a_reorg() {
    let (c, _tokens) = ctx();
    let evil = evil_bid(&c).await;
    assert!(view(&c, &evil).listed, "nothing proves a program yet: the placement is listed");
    let honest = bid_on(T3, SCALE, 240_000_000);
    let escrow = honest.escrow(WHOLE, 2).unwrap() as u64;
    let honest_tx = c.w.create_tx(AnyState::KobBid(honest), escrow, MAKER_A, 0);
    c.push(&[&honest_tx]).await;
    let honest_cov = c.w.cov(&honest_tx, 0);
    assert!(view(&c, &honest_cov).listed);
    // the honest ask creates the token's holdings: the chain proves T3
    let ask_cov = honest_ask(&c).await;
    let ev = view(&c, &evil);
    assert!(!ev.listed, "the placement of another program is no longer listed");
    assert_eq!(ev.unlisted_reason.as_deref(), Some("token_program_conflict"));
    assert!(view(&c, &ask_cov).listed && view(&c, &honest_cov).listed, "the orders of the proven program stay listed");
    let bids: i64 = c.hs.query("SELECT COUNT(*) FROM orders WHERE token_cov_id = ?1 AND side = 2 AND listed = 1", [&TOKEN_COV[..]]);
    assert_eq!(bids, 1);
    // the ask's block is reorged out: the proof goes and the placement is listed again, exactly as it was
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    let ev = view(&c, &evil);
    assert!(ev.listed && ev.unlisted_reason.is_none(), "{ev:?}");
}
