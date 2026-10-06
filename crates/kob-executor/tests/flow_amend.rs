//! In-place amend (AMEND record): the maker's cancel of a plain ask continues the order's covenant id with new terms and
//! the custody, owned by that id, never moves. The indexer proves the continuation against the revealed previous state
//! (`payload::verify_amend`) and takes the new terms into the order (price, tip, expiry, listing decision); a reorg
//! restores the old terms exactly. A continuation of a cancel that is not a verified amend (forged record, no record, a
//! second output carrying the id) stays unproven and is listed nowhere: neither in the matcher's book nor in the public
//! book, depth or counts. A plain bid is amended the same way (it owns no custody: its quantity is its escrow), and its
//! budget rate (and so the amount its escrow funds) follows the new terms.

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book;
use kob_executor::indexer::reads::{self, ReadCtx};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::*;
use kob_protocol::tx::*;

const P240: i64 = 240_000_000;

fn amend_req(c: &Ctx, at: &SignedTx, idx: usize, a: &AskState, amended: AskState) -> AmendOrder {
    AmendOrder {
        order: c.w.order(at, idx, AnyState::KobAsk(a.clone())),
        amended: AnyState::KobAsk(amended),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    }
}

fn read_ctx(c: &Ctx) -> ReadCtx {
    ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

/// Public book (KAS book of the token): the ask levels' prices.
fn ask_prices(c: &Ctx) -> Vec<String> {
    let g = c.hs.ingest.lock().unwrap();
    let b = reads::book(g.conn(), &read_ctx(c), &Hash32(TOKEN_COV), 50, false).unwrap();
    match b.asks {
        reads::BookSide::Orders(v) => v.into_iter().map(|o| o.price).collect(),
        reads::BookSide::Levels(v) => v.into_iter().map(|l| l.price).collect(),
    }
}

/// The amount a view reports left (a bid: the amount its escrow still funds at its budget rate), base units.
fn amount_of(v: &reads::OrderView) -> i64 {
    v.amount_left.as_deref().expect("amount left").parse().unwrap()
}

fn matcher_book(c: &Ctx) -> Vec<kob_executor::matcher::book::ListedOrder> {
    book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap()
}

async fn placed(c: &Ctx) -> (SignedTx, Hash32, AskState, TokenUtxo) {
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    (create, cov, a, custody)
}

#[tokio::test]
async fn an_amended_ask_keeps_its_id_and_custody_and_takes_the_new_terms() {
    let c = Ctx::new();
    let (create, cov, a, custody) = placed(&c).await;
    let amended = AskState { price: P240, tip: 2 * a.tip, ..a.clone() };
    let am = c.w.sign(&Action::AmendOrder(amend_req(&c, &create, 0, &a, amended.clone())));
    assert_eq!(am.tx.inputs.len(), 1, "no token input, no funding: the carrier pays");
    c.push(&[&am]).await;

    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(amended.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "amend"]);
    let d = c.hs.event_detail(&cov, "amend");
    assert_eq!(d["previous"]["price"], P250);
    let v = view(&c, &cov);
    assert!(v.state_known && v.listed);
    assert_eq!(v.price.as_deref(), Some("240000000"));
    assert_eq!(v.current.as_ref().map(|o| (o.txid.clone(), o.index)), Some((kob_executor::hex::encode(&am.tx.id), 0)));
    assert!(v.custody.as_ref().is_some_and(|k| k.ok), "the custody is the one placed with the order: {:?}", v.custody);
    // the matcher's book: the new state with the same custody UTXO
    let listed = matcher_book(&c);
    let o = listed.iter().find(|o| o.id() == cov.0).expect("listed");
    assert_eq!(o.order.state, AnyState::KobAsk(amended.clone()));
    assert_eq!(o.custody.as_ref().map(|k| k.utxo.outpoint()), Some(custody.utxo.outpoint()));
    assert!(o.custody_ok());
    assert_eq!(ask_prices(&c), ["240000000"]);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects", []), 0);

    // a fill of the amended order: the continuation with the new state, against the untouched custody
    let leg = Leg::Ask { order: c.w.order(&am, 0, amended.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.event_kinds(&cov), ["create", "amend", "fill"]);
    assert_eq!(c.hs.query::<i64>("SELECT price FROM order_events WHERE kind = 'fill'", []), P240);
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(AskState { amount_left: 6 * WHOLE, ..amended })));
}

#[tokio::test]
async fn a_reorg_restores_the_terms_an_amend_replaced() {
    let c = Ctx::new();
    let (create, cov, a, _) = placed(&c).await;
    let before = c.hs.snapshot();
    let amended = AskState { price: P240, expiry_daa: a.expiry_daa - 1_000, ..a.clone() };
    let am = c.w.sign(&Action::AmendOrder(amend_req(&c, &create, 0, &a, amended.clone())));
    c.push(&[&am]).await;
    assert_eq!(view(&c, &cov).price.as_deref(), Some("240000000"));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM order_amends", []), 1);
    // the amend's block is reorged out
    c.hs.node.reorg(1, vec![vec![]]);
    c.hs.sync().await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create"]);
    let v = view(&c, &cov);
    assert_eq!((v.price.as_deref(), v.expiry_daa), (Some("250000000"), Some(a.expiry_daa)));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM order_amends", []), 0);
    // every row is what it was before the amend (block rows aside: a replaced block is another block)
    let rows = |s: &str| s.lines().filter(|l| !l.starts_with("state ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows(&c.hs.snapshot()), rows(&before));
    assert_eq!(ask_prices(&c), ["250000000"]);
    // re-included, it applies again
    c.w.include(&c.hs.node, &[&am]);
    c.hs.sync().await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(amended)));
    assert_eq!(view(&c, &cov).price.as_deref(), Some("240000000"));
}

/// An amend re-runs the listing rules on the new terms (here: an order value below the operator's minimum unlists it).
#[tokio::test]
async fn an_amend_is_listed_by_the_rules_like_a_new_order() {
    let rules = kob_executor::tokens::ListingRules {
        min_order_value_sompi: 20 * KAS as i64,
        max_expiry_span_daa: 1 << 40,
        ..Default::default()
    };
    let proc = kob_executor::indexer::processor::Processor { tokens: std::sync::Arc::new(allowlist()), rules };
    let hs = Harness::with_processor(MockNode::new("testnet-10"), conn(), None, wide_window(), proc);
    let c = Ctx::with(hs);
    let (create, cov, a, _) = placed(&c).await;
    assert!(view(&c, &cov).listed);
    let cheap = AskState { price: 150_000_000, ..a.clone() };
    let am = c.w.sign(&Action::AmendOrder(amend_req(&c, &create, 0, &a, cheap.clone())));
    c.push(&[&am]).await;
    let v = view(&c, &cov);
    assert!(!v.listed, "10 whole tokens at 1.5 KAS (15 KAS) are below the 20 KAS minimum");
    assert!(v.unlisted_reason.is_some());
    assert!(matcher_book(&c).iter().all(|o| o.id() != cov.0));
    assert!(ask_prices(&c).is_empty());
    // amended back above the minimum, it is listed again
    let back = AskState { price: P250, ..cheap.clone() };
    let am2 = c.w.sign(&Action::AmendOrder(amend_req(&c, &am, 0, &cheap, back)));
    c.push(&[&am2]).await;
    assert!(view(&c, &cov).listed);
    assert_eq!(ask_prices(&c), ["250000000"]);
}

/// Re-signs a built amend after `edit` (the engine runs the whole transaction: it is consensus-valid).
fn hostile(c: &Ctx, req: AmendOrder, edit: impl FnOnce(&mut BuiltTx)) -> SignedTx {
    let mut built = build(&Action::AmendOrder(req)).unwrap();
    edit(&mut built);
    let (tx, entries) = built.tx.to_tx().unwrap();
    tx.set_storage_mass(masses(&tx, &entries).storage);
    built.tx = TxJson::from_tx(&tx, &entries);
    c.w.resign(built, true)
}

fn assert_listed_nowhere(c: &Ctx, cov: &Hash32, what: &str) {
    let v = view(c, cov);
    assert!(!v.state_known, "{what}: the continuation's state must stay unproven");
    assert!(matcher_book(c).iter().all(|o| o.id() != cov.0), "{what}: not in the matcher's book");
    assert!(ask_prices(c).is_empty(), "{what}: not in the public book");
    let g = c.hs.ingest.lock().unwrap();
    let t = reads::token_list(g.conn(), &allowlist()).unwrap();
    assert!(t.iter().all(|x| x.open_asks == 0), "{what}: not counted as an open ask");
}

#[tokio::test]
async fn a_cancel_continuation_that_is_not_a_verified_amend_is_listed_nowhere() {
    // (a) the record's state is not the output's
    {
        let c = Ctx::new();
        let (create, cov, a, _) = placed(&c).await;
        let req = amend_req(&c, &create, 0, &a, AskState { price: P240, ..a.clone() });
        let lie = AskState { price: 1, ..a.clone() };
        let t = hostile(&c, req, |b| b.tx.payload = payload::encode(&[Record::amend(0, 0, &AnyState::KobAsk(lie), None)]).unwrap());
        c.push(&[&t]).await;
        assert_listed_nowhere(&c, &cov, "forged state");
        assert!(c.hs.query::<String>("SELECT reason FROM rejects", []).starts_with("amend:"));
        assert_eq!(c.hs.event_kinds(&cov), ["create", "cancel"]);
    }
    // (b) no record at all: a cancel that continues the id with some script
    {
        let c = Ctx::new();
        let (create, cov, a, _) = placed(&c).await;
        let req = amend_req(&c, &create, 0, &a, AskState { price: P240, ..a.clone() });
        let t = hostile(&c, req, |b| b.tx.payload = vec![]);
        c.push(&[&t]).await;
        assert_listed_nowhere(&c, &cov, "no record");
    }
    // (c) a second output carrying the order's id (it could unlock the custody outside the order's rules)
    {
        let c = Ctx::new();
        let (create, cov, a, _) = placed(&c).await;
        let req = amend_req(&c, &create, 0, &a, AskState { price: P240, ..a.clone() });
        let t = hostile(&c, req, |b| {
            let mut twin = b.tx.outputs[0].clone();
            twin.value = KAS;
            b.tx.outputs[0].value -= KAS + 100_000; // and a little more fee for the extra output
            twin.script_public_key = spk_to_string(&kob_protocol::script::p2pk_spk(&pk(MAKER_A)));
            b.tx.outputs.push(twin);
        });
        assert_eq!(t.tx.outputs.iter().filter(|o| o.covenant.as_ref().is_some_and(|k| k.covenant_id == cov.0)).count(), 2);
        c.push(&[&t]).await;
        assert_listed_nowhere(&c, &cov, "two outputs carry the id");
        assert!(c.hs.query::<String>("SELECT reason FROM rejects", []).contains("other outputs carry"));
    }
    // (d) terms that would move the custody (another amount), output and record agreeing
    {
        let c = Ctx::new();
        let (create, cov, a, _) = placed(&c).await;
        let req = amend_req(&c, &create, 0, &a, AskState { price: P240, ..a.clone() });
        let moved = AnyState::KobAsk(AskState { amount_left: 5 * WHOLE, ..a.clone() });
        let t = hostile(&c, req, |b| {
            b.tx.outputs[0].script_public_key = spk_to_string(&moved.spk());
            b.tx.payload = payload::encode(&[Record::amend(0, 0, &moved, None)]).unwrap();
        });
        c.push(&[&t]).await;
        assert_listed_nowhere(&c, &cov, "amountLeft changed");
    }
}

/// Public book: the bid levels' prices.
fn bid_prices(c: &Ctx) -> Vec<String> {
    let g = c.hs.ingest.lock().unwrap();
    let b = reads::book(g.conn(), &read_ctx(c), &Hash32(TOKEN_COV), 50, false).unwrap();
    match b.bids {
        reads::BookSide::Orders(v) => v.into_iter().map(|o| o.price).collect(),
        reads::BookSide::Levels(v) => v.into_iter().map(|l| l.price).collect(),
    }
}

#[tokio::test]
async fn an_amended_bid_keeps_its_id_and_its_buying_power_follows_the_new_terms() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P250);
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let before = c.hs.snapshot();
    let amount_before = amount_of(&view(&c, &cov));
    // a lower price: the same escrow funds a larger amount
    let amended = BidState { price: P240 / 2, ..b0.clone() };
    let req = AmendOrder {
        order: c.w.order(&create, 0, AnyState::KobBid(b0.clone())),
        amended: AnyState::KobBid(amended.clone()),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let am = c.w.sign(&Action::AmendOrder(req));
    assert_eq!(am.tx.inputs.len(), 1, "the escrow pays the fee");
    c.push(&[&am]).await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects", []), 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(amended.clone())));
    assert_eq!(c.hs.event_kinds(&cov), ["create", "amend"]);
    let v = view(&c, &cov);
    assert!(v.state_known && v.listed);
    assert_eq!(v.price.as_deref(), Some("120000000"));
    assert_eq!(v.budget_rate.as_deref(), Some(amended.budget_rate().unwrap().to_string().as_str()));
    assert!(amount_of(&v) > amount_before, "{:?} -> {:?}", amount_before, v.amount_left);
    let listed = matcher_book(&c);
    let o = listed.iter().find(|o| o.id() == cov.0).expect("listed");
    assert_eq!(o.order.state, AnyState::KobBid(amended.clone()));
    assert_eq!(bid_prices(&c), ["120000000"]);

    // a fill of the amended bid from its continuation
    let mut b = batch(&c.w, c.daa(), vec![Leg::Bid { order: c.w.order(&am, 0, amended.clone()), amount: 2 * WHOLE, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 2 * WHOLE)];
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert_eq!(c.hs.query::<i64>("SELECT price FROM order_events WHERE kind = 'fill'", []), P240 / 2);

    // a reorg of both blocks restores the old terms, budget rate included
    c.hs.node.reorg(2, vec![vec![], vec![]]);
    c.hs.sync().await;
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobBid(b0.clone())));
    let v = view(&c, &cov);
    assert_eq!(v.budget_rate.as_deref(), Some(b0.budget_rate().unwrap().to_string().as_str()));
    assert_eq!(amount_of(&v), amount_before);
    let rows = |s: &str| s.lines().filter(|l| !l.starts_with("state ")).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(rows(&c.hs.snapshot()), rows(&before));
}

#[tokio::test]
async fn a_bid_amend_that_changes_the_scale_is_listed_nowhere() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P250);
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let req = AmendOrder {
        order: c.w.order(&create, 0, AnyState::KobBid(b0.clone())),
        amended: AnyState::KobBid(BidState { price: P240, ..b0.clone() }),
        value: None,
        funding: vec![],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let moved = AnyState::KobBid(BidState { scale: b0.scale * 10, ..b0.clone() });
    let t = hostile(&c, req, |b| {
        b.tx.outputs[0].script_public_key = spk_to_string(&moved.spk());
        b.tx.payload = payload::encode(&[Record::amend(0, 0, &moved, None)]).unwrap();
    });
    c.push(&[&t]).await;
    let v = view(&c, &cov);
    assert!(!v.state_known, "the continuation's state stays unproven");
    assert!(matcher_book(&c).iter().all(|o| o.id() != cov.0));
    assert!(bid_prices(&c).is_empty());
    assert!(c.hs.query::<String>("SELECT reason FROM rejects", []).starts_with("amend:"));
}
