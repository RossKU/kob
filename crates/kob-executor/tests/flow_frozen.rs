//! "Possibly frozen": an order flagged by the matcher's engine pre-simulation is left out of the books, the API says so, and the
//! flag lapses when the order moves or a later pre-simulation passes. (The matcher side is in `matcher_frozen.rs`.)

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::{flags, reads};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;

fn ctx_of(c: &Ctx) -> reads::ReadCtx {
    reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

fn asks_in_book(c: &Ctx) -> usize {
    let g = c.hs.ingest.lock().unwrap();
    let bk = reads::book(g.conn(), &ctx_of(c), &Hash32(TOKEN_COV), 10, false).unwrap();
    let reads::BookSide::Orders(asks) = bk.asks else { panic!() };
    asks.len()
}

fn open_asks_of_token(c: &Ctx) -> i64 {
    let g = c.hs.ingest.lock().unwrap();
    let tokens = kob_executor::testkit::allowlist();
    let list = reads::token_list(g.conn(), &tokens).unwrap();
    list.iter().find(|t| t.covenant_id == Hash32(TOKEN_COV).to_hex()).expect("the token is listed").open_asks
}

#[tokio::test]
async fn a_flagged_order_leaves_the_books_and_is_marked_in_the_api() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(asks_in_book(&c), 1);
    assert_eq!(open_asks_of_token(&c), 1);
    let v = view(&c, &cov);
    assert!(!v.possibly_frozen && v.frozen_reason.is_none());

    // the matcher's pre-simulation failed at the token program: flag it
    {
        let g = c.hs.ingest.lock().unwrap();
        assert!(flags::set(g.conn(), &cov.0, "the token program rejected the pre-simulated spend", c.daa()).unwrap());
        // an unknown order cannot be flagged
        assert!(!flags::set(g.conn(), &[9; 32], "x", 1).unwrap());
        assert_eq!(flags::reason(g.conn(), &cov.0).unwrap().as_deref(), Some("the token program rejected the pre-simulated spend"));
    }
    let v = view(&c, &cov);
    assert!(v.possibly_frozen, "the order view says so");
    assert!(v.frozen_reason.as_deref().unwrap().contains("token program"));
    assert!(v.listed && v.in_book, "it stays an indexed order (cancel and refund keep working)");
    assert_eq!(asks_in_book(&c), 0, "the book does not list it as live liquidity");
    assert_eq!(open_asks_of_token(&c), 0, "nor do the token counts");
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["possibly_frozen"], true);

    // a later pre-simulation passed: the flag is cleared and the order is live again
    {
        let g = c.hs.ingest.lock().unwrap();
        assert!(flags::clear(g.conn(), &cov.0).unwrap());
        assert!(!flags::clear(g.conn(), &cov.0).unwrap());
    }
    assert!(!view(&c, &cov).possibly_frozen);
    assert_eq!(asks_in_book(&c), 1);
}

#[tokio::test]
async fn a_flag_lapses_when_the_order_moves() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    {
        let g = c.hs.ingest.lock().unwrap();
        flags::set(g.conn(), &cov.0, "frozen?", c.daa()).unwrap();
    }
    assert!(view(&c, &cov).possibly_frozen);
    // the order is filled (the pre-simulation was wrong, or the freeze was lifted): a new UTXO carries no flag
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let v = view(&c, &cov);
    assert!(!v.possibly_frozen, "the flag belonged to the spent UTXO");
    assert_eq!(asks_in_book(&c), 1);
}

/// The runner's book source (the indexer) stores what the matcher reports: flagged orders leave the books, cleared ones return.
#[tokio::test]
async fn the_indexer_source_stores_and_clears_the_matchers_report() {
    use kob_executor::executor::IndexerSource;
    use kob_executor::matcher::run::BookSource;
    let c = Ctx::new();
    let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let mut src = IndexerSource::new(c.hs.ingest.clone());
    src.report_frozen(c.daa(), &[(cov.0, "token program refused".into())], &[]);
    assert!(view(&c, &cov).possibly_frozen);
    assert_eq!(asks_in_book(&c), 0);
    src.report_frozen(c.daa() + 3_000, &[], &[cov.0]);
    assert!(!view(&c, &cov).possibly_frozen);
    assert_eq!(asks_in_book(&c), 1);
}

/// The open token list end to end: with the registry's strict template list loaded, an order on a token that has NO registry entry is
/// listed because its program is on the list; it shows in `/v1/tokens` as unverified (no ticker), with the program's powers. A
/// program off the strict list is not listed.
#[tokio::test]
async fn the_open_token_list_lists_a_token_of_a_strict_program_without_an_entry() {
    use kob_executor::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
    use kob_protocol::artifacts::{token_template, TemplateId};

    let strict = |t: TemplateId, powers: &[&str]| {
        (
            Hash32(token_template(t).hash),
            StrictTemplate { id: t.name().into(), family: t.family(), powers: powers.iter().map(|s| s.to_string()).collect() },
        )
    };
    let run = |templates: Vec<(Hash32, StrictTemplate)>| async move {
        let tokens = TokenAllowlist::from_entries([]).with_strict_templates(templates);
        let mut proc = processor();
        proc.tokens = std::sync::Arc::new(tokens);
        proc.rules = ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() };
        let c = Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc));
        let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
        c.push(&[&create]).await;
        let cov = c.w.cov(&create, 0);
        let v = view(&c, &cov);
        let rows = {
            let g = c.hs.ingest.lock().unwrap();
            let tokens = TokenAllowlist::from_entries([]).with_strict_templates([strict(TemplateId::Kcc20Ref, &["freeze"])]);
            reads::token_list(g.conn(), &tokens).unwrap()
        };
        (v, rows)
    };
    // the fixtures' token program is the reference 3/3: on the strict list -> listed, unverified, powers from the template
    let (v, rows) = run(vec![strict(TemplateId::Kcc20Ref, &["freeze"])]).await;
    assert!(v.listed, "{:?}", v.unlisted_reason);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].standing, rows[0].ticker.as_str(), rows[0].family), ("unverified", "", Some("kcc20")));
    assert_eq!(rows[0].powers, vec!["freeze".to_string()]);
    assert_eq!((rows[0].open_asks, rows[0].template_id.as_deref()), (1, Some("KCC20Ref")));
    // another program on the list, the order's program is not: refused with the reason
    let (v, _) = run(vec![strict(TemplateId::KronToken2433, &[])]).await;
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("token_template_not_strict"));
}
