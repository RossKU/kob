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
        let stale = {
            let g = c.hs.ingest.lock().unwrap();
            let tokens = TokenAllowlist::from_entries([]).with_strict_templates([strict(TemplateId::KronToken2433, &[])]);
            reads::token_list(g.conn(), &tokens).unwrap()
        };
        (v, rows, stale)
    };
    // the fixtures' token program is the reference 3/3: on the strict list -> listed, powers from the template
    let (v, rows, stale) = run(vec![strict(TemplateId::Kcc20Ref, &["freeze"])]).await;
    assert!(v.listed, "{:?}", v.unlisted_reason);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].ticker.as_str(), rows[0].family), ("", Some("kcc20")));
    assert_eq!(rows[0].powers, vec!["freeze".to_string()]);
    assert_eq!((rows[0].open_asks, rows[0].template_id.as_deref()), (1, Some("KCC20Ref")));
    // the order stays listed in the history, but under a strict list that no longer has its program the token is not in the list
    assert!(stale.is_empty(), "{stale:?}");
    // another program on the list, the order's program is not: refused with the reason
    let (v, _, _) = run(vec![strict(TemplateId::KronToken2433, &[])]).await;
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("token_template_not_strict"));
}

/// The open token list also lists a token whose only order was NOT listed (here: below the minimum order value): the order's
/// placement recorded the token's identity (`token_events`), which proves its program, so the token is in `/v1/tokens` (no open
/// orders, no scale) and its first listed order can be placed from a client. The strict list still decides.
#[tokio::test]
async fn the_open_token_list_lists_a_token_known_only_from_an_unlisted_order() {
    use kob_executor::tokens::{ListingRules, StrictTemplate, TokenAllowlist};
    use kob_protocol::artifacts::{token_template, TemplateId};

    let strict =
        |t: TemplateId| (Hash32(token_template(t).hash), StrictTemplate { id: t.name().into(), family: t.family(), powers: vec![] });
    let tokens = TokenAllowlist::from_entries([]).with_strict_templates([strict(TemplateId::Kcc20Ref)]);
    let mut proc = processor();
    proc.tokens = std::sync::Arc::new(tokens);
    proc.rules = ListingRules { min_order_value_sompi: i64::MAX / 4, max_expiry_span_daa: 1 << 40, ..ListingRules::default() };
    let c = Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc));
    let create = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let v = view(&c, &c.w.cov(&create, 0));
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("order_value_below_minimum"));
    let list = |t: TemplateId| {
        let g = c.hs.ingest.lock().unwrap();
        reads::token_list(g.conn(), &TokenAllowlist::from_entries([]).with_strict_templates([strict(t)])).unwrap()
    };
    let rows = list(TemplateId::Kcc20Ref);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].covenant_id, Hash32(TOKEN_COV).to_hex());
    assert_eq!((rows[0].family, rows[0].scale), (Some("kcc20"), None));
    assert_eq!((rows[0].open_asks, rows[0].open_bids), (0, 0));
    // the one extension commitment its placements named (a client checks it against what it knows without the indexer)
    assert_eq!(rows[0].extension_commitment.as_deref(), Some(Hash32(EXT).to_hex().as_str()));
    assert_eq!(rows[0].template_hash.as_deref(), Some(Hash32(token_template(TemplateId::Kcc20Ref).hash).to_hex().as_str()));
    // a strict list without the token's program: not listed
    assert!(list(TemplateId::KronToken2433).is_empty());
    // a placement that named another commitment for the same token: no commitment is reported (a client reads its orders)
    {
        let g = c.hs.ingest.lock().unwrap();
        g.conn()
            .execute(
                "INSERT INTO token_events (token_cov_id, tpl_hash, ext_commit, kind, txid, block_seq, daa) VALUES (?1, ?2, ?3, 'seen', ?4, 1, 1)",
                rusqlite::params![&TOKEN_COV[..], &token_template(TemplateId::Kcc20Ref).hash[..], &[0xabu8; 32][..], &[0x77u8; 32][..]],
            )
            .unwrap();
    }
    let rows = list(TemplateId::Kcc20Ref);
    assert_eq!((rows.len(), rows[0].extension_commitment.as_deref()), (1, None));
}

/// The all-zero extension commitment (the standard fixed-supply KCC-20 class: TUSD, every `kob token issue`) is a commitment:
/// the open token list reports it (it reported none, and the CLI refused every such token: TN10 2026-10-14 B-4), and the
/// migration of earlier pair rows leaves a real zero commitment alone.
#[tokio::test]
async fn the_open_token_list_reports_the_zero_extension_commitment() {
    use kob_executor::tokens::{StrictTemplate, TokenAllowlist};
    use kob_protocol::artifacts::{token_template, TemplateId};

    let strict = (
        Hash32(token_template(TemplateId::Kcc20Ref).hash),
        StrictTemplate { id: "KCC20Ref".into(), family: TemplateId::Kcc20Ref.family(), powers: vec![] },
    );
    let c = Ctx::new();
    let b = BidState { extension_commitment: [0; 32], ..bid(MAKER_B, P250) };
    let create = c.w.create_tx(AnyState::KobBid(b.clone()), b.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let list = || {
        let g = c.hs.ingest.lock().unwrap();
        reads::token_list(g.conn(), &TokenAllowlist::from_entries([]).with_strict_templates([strict.clone()])).unwrap()
    };
    let rows = list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].extension_commitment.as_deref(), Some(Hash32([0; 32]).to_hex().as_str()));
    {
        let g = c.hs.ingest.lock().unwrap();
        assert_eq!(kob_executor::indexer::db::backfill_unknown_pair_extensions(g.conn()).unwrap(), 0);
    }
    assert_eq!(list()[0].extension_commitment.as_deref(), Some(Hash32([0; 32]).to_hex().as_str()));
}

/// The market scale of a token without decimals is sticky (TN10 2026-10-15 O-3): `/v1/tokens` reports the scale of the token's
/// earliest listed order after every order of it has ended, and a later order of another scale does not move it. A client prices
/// the next "first order" at it instead of asking for decimals again (another value would put two scales on one market).
#[tokio::test]
async fn the_open_token_list_reports_the_scale_of_the_earliest_order_after_it_ended() {
    use kob_executor::tokens::{StrictTemplate, TokenAllowlist};
    use kob_protocol::artifacts::{token_template, TemplateId};
    use kob_protocol::tx::FeeOptions;

    let strict = (
        Hash32(token_template(TemplateId::Kcc20Ref).hash),
        StrictTemplate { id: "KCC20Ref".into(), family: TemplateId::Kcc20Ref.family(), powers: vec![] },
    );
    let c = Ctx::new();
    let list = || {
        let g = c.hs.ingest.lock().unwrap();
        let rows = reads::token_list(g.conn(), &TokenAllowlist::from_entries([]).with_strict_templates([strict.clone()])).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows.into_iter().next().unwrap()
    };
    // the first order (scale 1000), then cancelled: no open order is left, the market keeps its scale
    let b = bid(MAKER_B, P250);
    let create = c.w.create_tx(AnyState::KobBid(b.clone()), b.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let first = c.w.cov(&create, 0);
    assert_eq!((list().open_bids, list().market_scale), (1, Some(SCALE)));
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order: c.w.order(&create, 0, AnyState::KobBid(b.clone())),
        custody: None,
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_B, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&first), "cancelled");
    let row = list();
    assert_eq!((row.open_bids, row.scale, row.market_scale), (0, None, Some(SCALE)));
    assert_eq!(serde_json::to_value(&row).unwrap()["market_scale"], SCALE);
    // a later order of another scale does not move it
    let b2 = BidState { scale: 100, min_fill: 100, ..bid(MAKER_B, P250) };
    let create2 = c.w.create_tx(AnyState::KobBid(b2.clone()), b2.escrow(500, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create2]).await;
    assert_eq!((list().open_bids, list().market_scale), (1, Some(SCALE)));
    // only listed orders set it while one is among the token's oldest; none listed: the earliest order's
    let g = || c.hs.ingest.lock().unwrap();
    g().conn().execute("UPDATE orders SET listed = 0 WHERE covenant_id = ?1", [&first.0[..]]).unwrap();
    assert_eq!(list().market_scale, Some(100));
    g().conn().execute("UPDATE orders SET listed = 0", []).unwrap();
    assert_eq!(list().market_scale, Some(SCALE));
    // a token without an order (known from its placements' token events) has none
    g().conn().execute("DELETE FROM orders", []).unwrap();
    assert_eq!(list().market_scale, None);
}
