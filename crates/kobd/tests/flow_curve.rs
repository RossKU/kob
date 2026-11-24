//! `KobCurveAsk` in the indexer (`docs/design/curve-ask-spec.md`): placed and listed, shown as its
//! remaining levels (one order, its amount counted once), filled across levels (the event at the
//! mean level price, the view at the current level), amended, cancelled, reverted by a reorg and
//! replayed from the record log; created unbound in its token's genesis (the KOB1 payload's GENESIS
//! record, the custody's token, one custody candidate, no KCC-23 listing of the curve), bound by
//! its first fill or by an amend that names its token.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::issue::{build_creation, CreationPlan, FundingUtxo, GenesisCurve, Holder, IssueSpec};
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::Hash32;
use kobd::indexer::book;
use kobd::indexer::db::open_memory;
use kobd::indexer::reads::{self, BookSide, HoldingFilter, ReadCtx};
use kobd::testkit::*;
use kobd::tokens::{ListingRules, TokenAllowlist, TokenEntry};
use std::sync::Arc;

/// Twelve levels of one whole token, from 2.50 KAS up by 0.025 KAS each.
fn table() -> Vec<i64> {
    (0..12).map(|i| P250 + i * 2_500_000).collect()
}

fn curve(maker: u8) -> CurveAskState {
    let (h, p, s) = tok_fields(T3);
    CurveAskState {
        maker: pk(maker),
        token_cov_id: TOKEN_COV,
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        scale: SCALE,
        min_fill: 1,
        tip: 0,
        active_from: 0,
        level_size: WHOLE,
        total: 12 * WHOLE,
        amount_left: 12 * WHOLE,
        extension_commitment: EXT,
        table: table(),
    }
}

fn read_ctx(c: &Ctx) -> ReadCtx {
    ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

/// The aggregated asks of the fixture token: (price, amount, orders).
fn ask_levels(c: &Ctx, ctx: &ReadCtx) -> Vec<(i64, i64, i64)> {
    let g = c.hs.ingest.lock().unwrap();
    match reads::book(g.conn(), ctx, &Hash32(TOKEN_COV), 50, true).unwrap().asks {
        BookSide::Levels(v) => v.into_iter().map(|l| (l.price.parse().unwrap(), l.amount.parse().unwrap(), l.orders)).collect(),
        BookSide::Orders(_) => unreachable!(),
    }
}

/// The per-order asks: (covenant id, curve level, price, amount).
fn ask_rows(c: &Ctx) -> Vec<(String, Option<i64>, i64, i64)> {
    let g = c.hs.ingest.lock().unwrap();
    match reads::book(g.conn(), &read_ctx(c), &Hash32(TOKEN_COV), 50, false).unwrap().asks {
        BookSide::Orders(v) => v
            .into_iter()
            .map(|o| (o.covenant_id, o.curve_level, o.price.parse().unwrap(), o.amount_left.unwrap().parse().unwrap()))
            .collect(),
        BookSide::Levels(_) => unreachable!(),
    }
}

async fn placed(c: &Ctx, s: &CurveAskState) -> (SignedTx, Hash32, TokenUtxo) {
    let create = c.w.create_tx(AnyState::KobCurveAsk(s.clone()), CARRIER, MAKER_A, s.amount_left);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let k = find_custody(&create, &cov, s.amount_left).expect("the custody");
    (create.clone(), cov, c.w.token_at(&create, k, Kcc20State::custody(s.amount_left, cov.0, EXT)))
}

/// A taker's fill of `n` from the curve at output 0 of `at`.
fn take(c: &Ctx, at: &SignedTx, s: &CurveAskState, custody: TokenUtxo, n: i64) -> SignedTx {
    let leg = Leg::CurveAsk { order: c.w.order(at, 0, s.clone()), custody, amount: n };
    c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])))
}

#[tokio::test]
async fn a_placed_curve_is_listed_as_its_levels_and_filled_across_them() {
    let c = Ctx::new();
    let s = curve(MAKER_A);
    let (create, cov, custody) = placed(&c, &s).await;
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects", []), 0);
    let v = view(&c, &cov);
    assert!(v.listed && v.state_known && v.in_book, "{:?}", v.unlisted_reason);
    assert_eq!((v.price.as_deref(), v.quote.as_deref(), v.expiry_daa), (Some("250000000"), Some("250000000"), None));
    let cv = v.curve.as_ref().expect("the curve view");
    assert_eq!((cv.level, cv.levels, cv.max_fill.as_str(), cv.unbound), (0, 12, "8000", false));
    assert_eq!(cv.remaining.len(), 12);
    assert!(v.custody.as_ref().is_some_and(|k| k.ok));
    // the matcher's book: the curve with its custody
    let listed = book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap();
    let o = listed.iter().find(|o| o.id() == cov.0).expect("listed");
    assert!(o.custody_ok());
    // the public book: one level per table entry, one order each, the amounts summing to amountLeft (counted once)
    let levels = ask_levels(&c, &read_ctx(&c));
    assert_eq!(levels, table().into_iter().map(|p| (p, WHOLE, 1)).collect::<Vec<_>>());
    let rows = ask_rows(&c);
    assert_eq!(rows.len(), 12);
    assert!(rows.iter().all(|r| r.0 == cov.to_hex()), "every row is the one order");
    assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), (0..12).map(Some).collect::<Vec<_>>());

    // a fill of 2.5 levels: levels 0 and 1 whole, half of level 2
    let n = 2 * WHOLE + WHOLE / 2;
    let fill = take(&c, &create, &s, custody, n);
    c.push(&[&fill]).await;
    let after = CurveAskState { amount_left: s.amount_left - n, ..s.clone() };
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(after.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "fill"]);
    let t = table();
    let mean = (WHOLE as i128 * t[0] as i128 + WHOLE as i128 * t[1] as i128 + (WHOLE / 2) as i128 * t[2] as i128) / n as i128;
    assert_eq!(c.hs.query::<i64>("SELECT price FROM order_events WHERE kind = 'fill'", []) as i128, mean, "the mean level price");
    assert_eq!(
        c.hs.query::<i64>("SELECT payout FROM order_events WHERE kind = 'fill'", []),
        s.proceeds(n).unwrap(),
        "the maker's proceeds"
    );
    let v = view(&c, &cov);
    assert_eq!(v.price, Some(t[2].to_string()), "the current level's price");
    let cv = v.curve.unwrap();
    assert_eq!((cv.level, cv.remaining[0].amount.as_str(), cv.remaining.len()), (2, "500", 10));
    assert!(v.custody.as_ref().is_some_and(|k| k.ok), "the rest custody is the order's: {:?}", v.custody);
    let levels = ask_levels(&c, &read_ctx(&c));
    assert_eq!(levels[0], (t[2], WHOLE / 2, 1));
    assert_eq!(levels.iter().map(|l| l.1).sum::<i64>(), after.amount_left);
    let listed = book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap();
    assert!(listed
        .iter()
        .find(|o| o.id() == cov.0)
        .is_some_and(|o| o.custody_ok() && o.order.state == AnyState::KobCurveAsk(after.clone())));
}

#[tokio::test]
async fn a_reorg_reverts_a_curve_fill_and_the_record_log_replays_the_curve() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = kobd::indexer::recordlog::RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(conn(), Some(log)));
    let s = curve(MAKER_A);
    let (create, cov, custody) = placed(&c, &s).await;
    let before = c.hs.snapshot();
    let levels_before = ask_levels(&c, &read_ctx(&c));
    let fill = take(&c, &create, &s, custody.clone(), 3 * WHOLE);
    c.push(&[&fill]).await;
    assert_eq!(view(&c, &cov).price, Some(table()[3].to_string()));
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(s.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create"]);
    assert_eq!(view(&c, &cov).price, Some(table()[0].to_string()));
    let rows = |x: &str| x.lines().filter(|l| !l.starts_with("state ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows(&c.hs.snapshot()), rows(&before));
    assert_eq!(ask_levels(&c, &read_ctx(&c)), levels_before);
    // fill again; the store rebuilt from the record log is the live one
    c.w.include(&c.hs.node, &[&fill]);
    c.hs.sync().await;
    assert_eq!(c.hs.status(&cov), "partial");
    let live = c.hs.snapshot();
    drop(c);
    let mut ing = kobd::indexer::ingest::Ingest::new(open_memory("testnet-10").unwrap(), processor(), None).with_config(wide_window());
    let logged = kobd::indexer::recordlog::read_all(dir.path()).unwrap();
    ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    assert_eq!(snapshot(ing.conn()), live);
}

#[tokio::test]
async fn a_curve_is_amended_in_place_and_cancelled() {
    let c = Ctx::new();
    let s = curve(MAKER_A);
    let (create, cov, custody) = placed(&c, &s).await;
    let amended = CurveAskState { table: table().iter().map(|p| p * 11 / 10).collect(), tip: TIP, ..s.clone() };
    let am = c.w.sign(&Action::AmendOrder(AmendOrder {
        order: c.w.order(&create, 0, AnyState::KobCurveAsk(s.clone())),
        amended: AnyState::KobCurveAsk(amended.clone()),
        value: None,
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
        custody: None,
    }));
    c.push(&[&am]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(amended.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "amend"]);
    let v = view(&c, &cov);
    assert!(v.listed);
    assert_eq!(v.price, Some((P250 * 11 / 10).to_string()));
    assert_eq!(ask_levels(&c, &read_ctx(&c))[0].0, P250 * 11 / 10);
    // the maker's cancel: the custody goes back, the order closes and leaves the book
    let k = find_cov(&am, &cov).unwrap();
    let cancel = c.w.sign(&Action::CancelOrder(CancelOrder {
        retired: None,
        prefund: None,
        order: c.w.order(&am, k, AnyState::KobCurveAsk(amended)),
        custody: Some(custody),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }));
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert!(ask_levels(&c, &read_ctx(&c)).is_empty());
}

#[tokio::test]
async fn a_curve_is_never_idle() {
    let c = Ctx::new();
    let (_, cov, _) = placed(&c, &curve(MAKER_A)).await;
    let a = c.w.create_tx(AnyState::KobAsk(ask(MAKER_B, P250 * 3)), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&a]).await;
    // 90 days on: a plain ask waits for its refund, the curve (no refund) is still liquidity
    let later = ReadCtx { node_daa: Some(c.daa() + MAX_IDLE as u64 + 10), settle_depth_daa: 100, now_unix: None };
    let levels = ask_levels(&c, &later);
    assert_eq!(levels.len(), 12, "only the curve's levels: {levels:?}");
    assert!(levels.iter().all(|l| l.0 != P250 * 3));
    assert_eq!(ask_levels(&c, &read_ctx(&c)).len(), 13, "before, both");
    assert!(view(&c, &cov).refund_due_daa.is_none());
}

// ---------------------------------------------------------------------------------------------
// created in its token's genesis

const CREATOR: u8 = MAKER_A;
const ON_CURVE: u64 = 600;

/// A token whose genesis places a curve over 600 of its 1,000 whole tokens; `extra`: a second holder owned by the curve.
fn creation(extra: bool) -> (CreationPlan, CurveAskState, Hash32, Hash32) {
    let funding = FundingUtxo {
        outpoint: kaspa_consensus_core::tx::TransactionOutpoint::new(kaspa_consensus_core::Hash::from_bytes([0x5c; 32]), 0),
        amount: 1_000 * KAS,
        owner_pubkey: pk(CREATOR),
    };
    let w = WHOLE as u64;
    let mut holders = vec![Holder::new([0; 32], 4, ON_CURVE * w), Holder::new(pk(CREATOR), 0, 400 * w)];
    let mut spec = IssueSpec::new("Curve Token", "CURVE", 3, 1_000 * w, holders.clone(), vec![funding]);
    let tpl = spec.program.template_id();
    let (h, p, s) = tok_fields(tpl);
    let state = CurveAskState {
        token_cov_id: [0; 32],
        token_tpl_hash: h,
        tpl_prefix_len: p,
        tpl_suffix_len: s,
        level_size: 60 * WHOLE,
        total: ON_CURVE as i64 * WHOLE,
        amount_left: ON_CURVE as i64 * WHOLE,
        extension_commitment: spec.extension_commitment,
        ..curve(CREATOR)
    };
    spec.curve = Some(GenesisCurve { state: state.clone(), custody: 0, carrier: CARRIER });
    if extra {
        // the curve's id: its output (after the holders) alone, from the outpoint the genesis spends (the
        // preparation's, which the holders' owners do not change)
        holders[1].amount -= w;
        holders.push(Holder::new([1; 32], 4, w));
        spec.holders = holders.clone();
        let first = build_creation(&spec).unwrap_or_else(|e| panic!("creation: {e}"));
        let out = kaspa_consensus_core::tx::TransactionOutput { value: CARRIER, script_public_key: state.spk(), covenant: None };
        let at = first.genesis.tx.inputs[0].previous_outpoint;
        let id = kaspa_consensus_core::hashing::covenant_id::covenant_id(at, [(3u32, &out)].into_iter());
        holders[2].owner = id.as_bytes();
        spec.holders = holders;
    }
    let mut plan = build_creation(&spec).unwrap_or_else(|e| panic!("creation: {e}"));
    plan.sign(&secp256k1::SecretKey::from_slice(&sk(CREATOR)).unwrap()).unwrap();
    let token = Hash32(plan.covenant_id().as_bytes());
    let curve_cov = Hash32(plan.genesis.curve.as_ref().expect("the curve").covenant_id.as_bytes());
    if extra {
        assert_eq!(plan.genesis.states[2].owner, curve_cov.0, "the second holder is owned by the curve");
    }
    (plan, state, token, curve_cov)
}

/// A harness that lists any token (open listing).
fn open_ctx() -> Ctx {
    let proc = kobd::indexer::processor::Processor {
        tokens: Arc::new(TokenAllowlist::from_entries(Vec::<TokenEntry>::new())),
        rules: ListingRules {
            require_allowlist: false,
            min_order_value_sompi: 1,
            max_expiry_span_daa: 1 << 40,
            ..ListingRules::default()
        },
    };
    Ctx::with(Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc))
}

async fn launch(c: &Ctx, plan: &CreationPlan) {
    c.hs.node.push_block(vec![kcc23_wire(&plan.prep.tx, true), kcc23_wire(&plan.genesis.tx, true)]);
    c.hs.sync().await;
}

/// The curve's order UTXO and custody in the genesis.
fn genesis_utxos(
    c: &Ctx,
    plan: &CreationPlan,
    s: &CurveAskState,
    token: Hash32,
    cov: Hash32,
) -> (OrderUtxo<CurveAskState>, TokenUtxo) {
    let g = plan.genesis.txid().as_bytes();
    let at = plan.genesis.curve.as_ref().unwrap().output as u32;
    let order = OrderUtxo {
        utxo: Utxo { transaction_id: g, index: at, amount: CARRIER, block_daa_score: c.daa(), covenant_id: Some(cov.0) },
        state: s.clone(),
    };
    let custody = TokenUtxo {
        utxo: Utxo {
            transaction_id: g,
            index: 0,
            amount: plan.genesis.spec.carrier,
            block_daa_score: c.daa(),
            covenant_id: Some(token.0),
        },
        state: TokenState::custody(kob_protocol::family::Family::Kcc20, s.amount_left, cov.0, s.extension_commitment),
    };
    (order, custody)
}

#[tokio::test]
async fn a_curve_created_in_its_token_genesis_is_listed_and_bound_by_its_first_fill() {
    let c = open_ctx();
    let (plan, s, token, cov) = creation(false);
    launch(&c, &plan).await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects", []), 0);
    let v = view(&c, &cov);
    assert!(v.listed && v.state_known, "{:?}", v.unlisted_reason);
    assert_eq!(v.token, Some(token.to_hex()), "the custody's token");
    assert!(v.curve.as_ref().unwrap().unbound);
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(s.clone())), "the state on chain names no token");
    // the genesis outputs are holdings (the KOB1 payload's GENESIS record): the creator's 400 and the curve's custody
    let ctx = read_ctx(&c);
    let list = |owner: [u8; 32]| {
        let g = c.hs.ingest.lock().unwrap();
        let f = HoldingFilter { owner: Some(owner.to_vec()), token: None, include_spent: false };
        reads::holdings(g.conn(), &ctx, &f, 50, None).unwrap().items
    };
    assert_eq!(list(pk(CREATOR)).iter().map(|h| h.amount.clone()).collect::<Vec<_>>(), [(400 * WHOLE).to_string()]);
    assert_eq!(
        list(cov.0).iter().map(|h| (h.amount.clone(), h.role.clone())).collect::<Vec<_>>(),
        [(s.total.to_string(), "custody".to_string())]
    );
    // KCC-23 declares the token, not the curve
    {
        let g = c.hs.ingest.lock().unwrap();
        assert!(kobd::indexer::kcc23::declared(g.conn(), &token).unwrap());
        assert!(!kobd::indexer::kcc23::declared(g.conn(), &cov).unwrap());
    }
    let listed = book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap();
    assert!(listed.iter().find(|o| o.id() == cov.0).is_some_and(|o| o.custody_ok()), "the matcher's book offers it");
    // the first fill binds it
    let (order, custody) = genesis_utxos(&c, &plan, &s, token, cov);
    let leg = Leg::CurveAsk { order, custody, amount: 5 * WHOLE };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let bound = CurveAskState { token_cov_id: token.0, amount_left: s.amount_left - 5 * WHOLE, ..s };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(bound)));
    let v = view(&c, &cov);
    assert_eq!(c.hs.status(&cov), "partial");
    assert!(!v.curve.unwrap().unbound && v.custody.is_some_and(|k| k.ok));
}

#[tokio::test]
async fn a_genesis_curve_with_a_second_custody_candidate_is_not_listed() {
    let c = open_ctx();
    let (plan, _, _, cov) = creation(true);
    launch(&c, &plan).await;
    let v = view(&c, &cov);
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("curve_custody_ambiguous"));
}

fn amend_to(c: &Ctx, order: OrderUtxo<CurveAskState>, amended: CurveAskState, custody: TokenUtxo) -> SignedTx {
    c.w.sign(&Action::AmendOrder(AmendOrder {
        order: OrderUtxo { utxo: order.utxo, state: AnyState::KobCurveAsk(order.state) },
        amended: AnyState::KobCurveAsk(amended),
        value: None,
        funding: vec![c.w.coin(CREATOR, 10)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
        custody: Some(custody),
    }))
}

#[tokio::test]
async fn an_amend_of_an_unbound_curve_must_bind_it_to_its_token() {
    let (plan, s, token, cov) = creation(false);
    // naming another token: a reject, the continuation listed nowhere
    let c = open_ctx();
    launch(&c, &plan).await;
    let (order, custody) = genesis_utxos(&c, &plan, &s, token, cov);
    // the builder checks the custody it is given: told it is of another token, it builds the wrong binding,
    // which the indexer must refuse
    let lie = TokenUtxo { utxo: Utxo { covenant_id: Some(TOKEN_COV), ..custody.utxo.clone() }, ..custody.clone() };
    let wrong = amend_to(&c, order, CurveAskState { token_cov_id: TOKEN_COV, tip: TIP, ..s.clone() }, lie);
    c.push(&[&wrong]).await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects WHERE reason = 'amend:curve_not_bound_to_its_token'", []), 1);
    assert!(!view(&c, &cov).state_known);
    // naming its token: bound
    let c = open_ctx();
    launch(&c, &plan).await;
    let (order, custody) = genesis_utxos(&c, &plan, &s, token, cov);
    let bound = CurveAskState { token_cov_id: token.0, tip: TIP, ..s.clone() };
    let right = amend_to(&c, order, bound.clone(), custody);
    c.push(&[&right]).await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobCurveAsk(bound)));
    let v = view(&c, &cov);
    assert!(v.listed && !v.curve.unwrap().unbound);
}

#[tokio::test]
async fn the_curve_view_serialises_its_levels() {
    let c = Ctx::new();
    let (_, cov, _) = placed(&c, &curve(MAKER_A)).await;
    let j = serde_json::to_value(view(&c, &cov)).unwrap();
    assert_eq!(j["contract"], "KobCurveAsk");
    assert_eq!(j["price"], "250000000");
    assert!(j["expiry_daa"].is_null());
    let cv = &j["curve"];
    for (k, want) in [
        ("level", "0"),
        ("levels", "12"),
        ("level_size", "\"1000\""),
        ("total", "\"12000\""),
        ("price_now", "\"250000000\""),
        ("max_fill", "\"8000\""),
        ("unbound", "false"),
    ] {
        assert_eq!(cv[k].to_string(), want, "{k}");
    }
    assert_eq!(cv["remaining"][1], serde_json::json!({"level": 1, "price": "252500000", "amount": "1000"}));
    let g = c.hs.ingest.lock().unwrap();
    let b = serde_json::to_value(reads::book(g.conn(), &read_ctx(&c), &Hash32(TOKEN_COV), 3, false).unwrap()).unwrap();
    assert_eq!(b["asks"][2]["curve_level"], 2);
    assert_eq!(b["asks"][2]["covenant_id"], cov.to_hex());
}

/// A book of depth k holds at most k levels of a curve, and its top k (per order and aggregated)
/// are exact; an order view lists at most 50 remaining levels.
#[tokio::test]
async fn a_shallow_book_and_a_view_take_only_the_best_levels_of_a_long_curve() {
    let c = Ctx::new();
    // 120 levels, a few equal prices in a row; and a plain ask between them
    let table: Vec<i64> = (0..120).map(|i| P250 + (i / 3) * 2_500_000).collect();
    let s = CurveAskState { table, total: 120 * WHOLE, amount_left: 120 * WHOLE, ..curve(MAKER_A) };
    let (_, cov, _) = placed(&c, &s).await;
    let a = ask(MAKER_B, P250 + 2_500_000);
    let plain = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_B, 10 * WHOLE);
    c.push(&[&plain]).await;
    let g = c.hs.ingest.lock().unwrap();
    let ctx = read_ctx(&c);
    let rows = |depth| match reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), depth, false).unwrap().asks {
        BookSide::Orders(v) => v.into_iter().map(|o| (o.curve_level, o.price.parse::<i64>().unwrap())).collect::<Vec<_>>(),
        BookSide::Levels(_) => unreachable!(),
    };
    let levels = |depth| match reads::book(g.conn(), &ctx, &Hash32(TOKEN_COV), depth, true).unwrap().asks {
        BookSide::Levels(v) => {
            v.into_iter().map(|l| (l.price.parse::<i64>().unwrap(), l.amount.parse::<i64>().unwrap())).collect::<Vec<_>>()
        }
        BookSide::Orders(_) => unreachable!(),
    };
    // levels 0-2 at 2.50, 3-5 at 2.525 (the older curve before the plain ask at that price), 6 at 2.55
    let seven = rows(7);
    let curve_level = |l: i64| (Some(l), P250 + (l / 3) * 2_500_000);
    let want: Vec<_> = (0..6).map(curve_level).chain([(None, P250 + 2_500_000)]).collect();
    assert_eq!(seven, want);
    assert_eq!(rows(5), want[..5].to_vec());
    let all = rows(200);
    assert_eq!(all.len(), 121);
    assert_eq!(&all[..7], &seven[..]);
    assert_eq!(all[7], curve_level(6));
    // aggregated: the curve's three levels at a price are one level, the plain ask's tokens added at its price
    assert_eq!(levels(3), vec![(P250, 3 * WHOLE), (P250 + 2_500_000, 13 * WHOLE), (P250 + 5_000_000, 3 * WHOLE)]);
    assert_eq!(&levels(100)[..3], &levels(3)[..]);
    let v = reads::order(g.conn(), &ctx, &cov).unwrap().unwrap();
    let cv = v.curve.unwrap();
    assert_eq!((cv.levels, cv.remaining.len()), (120, reads::CURVE_VIEW_LEVELS));
}
