//! C5 liveness audit: behaviour the audit found CORRECT, pinned so a later change cannot break it.
//!
//! 1. A sell-first repeating `KobIfdAsk` with `amountLeft = 0` (no custody): the view serves its decoded state, the entry's
//!    extension commitment, `custody { ok, utxo: null }` and `refund_due_daa`; the matcher's listed order has no custody and
//!    the keeper plans a `close` at exactly that DAA (not one DAA earlier).
//! 2. An order of a token that is NOT on the allowlist is unlisted (never offered to the matcher / keepers) but still
//!    indexed: its view carries the proven custody state and the maker's cancel built from the view alone validates in the
//!    script engine (docs/ops/executor.md: "Unlisted orders are still indexed and queryable by covenant id").

mod common;

use std::sync::Arc;

use common::*;
use kob_executor::hex::{self, Hash32};
use kob_executor::indexer::book::listed_orders;
use kob_executor::indexer::db::open_memory;
use kob_executor::indexer::processor::Processor;
use kob_executor::indexer::reads::{OrderView, TokenUtxoView};
use kob_executor::keepers::{refund_job, JobKind};
use kob_executor::testkit::*;
use kob_executor::tokens::{ListingRules, TokenAllowlist};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

fn h32(s: &str) -> [u8; 32] {
    *Hash32::parse(s).expect("32-byte hex").as_bytes()
}

fn token_from_view(v: &TokenUtxoView) -> TokenUtxo {
    let state = v.state.clone().unwrap_or_else(|| panic!("token UTXO {}:{} has no proven state in the view", v.txid, v.index));
    TokenUtxo {
        utxo: Utxo {
            transaction_id: h32(&v.txid),
            index: v.index as u32,
            amount: v.value.parse().unwrap(),
            block_daa_score: v.created_daa as u64,
            covenant_id: Some(h32(&v.token)),
        },
        state: serde_json::from_value(state).expect("the served token state is the builders' JSON"),
    }
}

/// The cancel of an order built from nothing but its indexer view.
fn cancel_from_view(c: &Ctx, v: &OrderView) -> SignedTx {
    assert!(v.state_known, "{}: state not proven", v.covenant_id);
    let cur = v.current.as_ref().expect("a live order has a current outpoint");
    let state: AnyState = serde_json::from_value(v.state.clone().expect("state")).expect("the served order state decodes");
    let order = OrderUtxo {
        utxo: Utxo {
            transaction_id: h32(&cur.txid),
            index: cur.index as u32,
            amount: cur.value.as_deref().expect("carrier").parse().unwrap(),
            block_daa_score: v.current_daa.expect("utxo daa") as u64,
            covenant_id: Some(h32(&v.covenant_id)),
        },
        state,
    };
    let custody = v.custody.as_ref().and_then(|cv| cv.utxo.as_ref()).map(token_from_view);
    let strays = v.strays.as_deref().unwrap_or_default().iter().map(token_from_view).collect();
    c.w.sign(&Action::CancelOrder(CancelOrder {
        prefund: None,
        order,
        custody,
        foreign: vec![],
        strays,
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    }))
}

#[tokio::test]
async fn an_empty_repeating_sell_first_entry_is_viewable_listed_and_closable_at_its_refund_time() {
    let c = Ctx::new();
    let ia = IfdAskState { rpt_amount: 1 + 20 * WHOLE, min_fill: 3 * WHOLE, ..ifd_ask(MAKER_A) };
    let create = c.w.create_tx(AnyState::KobIfdAsk(ia.clone()), ia.escrow(CARRIER as i64).unwrap() as u64, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::IfdAsk { order: c.w.order(&create, 0, ia.clone()), custody, amount: 10 * WHOLE, evidence: None, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    let waiting = IfdAskState { amount_left: 0, rpt_amount: 1 + 10 * WHOLE, ..ia.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobIfdAsk(waiting.clone())));

    let utxo_daa = c.w.utxo(&fill, find_cov(&fill, &cov).unwrap()).block_daa_score as i64;
    let due = AnyState::KobIfdAsk(waiting).refund_due(utxo_daa).unwrap();

    // the view: state, extension commitment, no custody needed, the refund time of the CURRENT UTXO
    let v = view(&c, &cov);
    assert!(v.state_known && v.state.is_some());
    assert_eq!(v.extension_commitment, Some(hex::encode(&EXT)));
    assert_eq!(v.current_daa, Some(utxo_daa));
    assert_eq!(v.refund_due_daa, Some(due), "idle / expiry time counted from the continuation's own DAA");
    assert_eq!(v.kill_daa, None);
    assert_eq!(v.amount_left.as_deref(), Some("0"));
    let cv = v.custody.expect("ask-side view");
    assert!(cv.ok && cv.utxo.is_none() && cv.expected_amount.as_deref() == Some("0"), "{cv:?}");

    // the matcher / keeper view: listed, no custody, closed (not refunded) at exactly `due`
    let listed = {
        let g = c.hs.ingest.lock().unwrap();
        listed_orders(g.conn()).unwrap()
    };
    let o = listed.iter().find(|o| o.id() == cov.0).expect("an empty repeating entry stays listed for its keeper");
    assert!(o.custody.is_none() && o.custody_ok());
    assert_eq!(refund_job(o, due as u64), Some((JobKind::Close, due as u64)));
    assert_eq!(refund_job(o, due as u64 - 1), None);
}

#[tokio::test]
async fn an_unlisted_order_stays_viewable_with_its_custody_state_and_cancels_from_the_view() {
    // an empty allowlist lists nothing: the order is `unlisted` with `token_not_allowlisted`
    let proc = Processor {
        tokens: Arc::new(TokenAllowlist::from_entries([])),
        rules: ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() },
    };
    let hs = Harness::with_processor(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), None, wide_window(), proc);
    let c = Ctx::with(hs);
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a partial fill moves the order and its custody: the continuation custody is classified and proven all the same
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;

    let v = view(&c, &cov);
    assert!(!v.listed);
    assert_eq!(v.unlisted_reason.as_deref(), Some("token_not_allowlisted"));
    assert_eq!(v.status, "partial");
    let cv = v.custody.as_ref().expect("ask-side view");
    assert!(cv.ok, "{cv:?}");
    assert!(cv.utxo.as_ref().is_some_and(|u| u.state.is_some()), "the custody carries its proven state: {cv:?}");
    assert!(listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap().is_empty(), "never offered to the matcher / keepers");

    let cancel = cancel_from_view(&c, &v); // engine-validated by World::sign
    c.push(&[&cancel]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
}

#[tokio::test]
async fn an_unlisted_ioc_order_is_offered_to_the_keepers_for_its_kill() {
    // K-4: the keepers serve proven unlisted orders too (kill / refund / close only, never matched): `unlisted_orders` is what
    // the indexer source hands them besides the listed book
    let proc = Processor {
        tokens: Arc::new(TokenAllowlist::from_entries([])),
        rules: ListingRules { min_order_value_sompi: 1, max_expiry_span_daa: 1 << 40, ..ListingRules::default() },
    };
    let hs = Harness::with_processor(MockNode::new("testnet-10"), open_memory("testnet-10").unwrap(), None, wide_window(), proc);
    let c = Ctx::with(hs);
    let a = AskState { tif: TIF_IOC, expiry_daa: NO_EXPIRY, ..ask(MAKER_A, P250) };
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert!(!view(&c, &cov).listed);
    let (listed, unlisted) = {
        let g = c.hs.ingest.lock().unwrap();
        (listed_orders(g.conn()).unwrap(), kob_executor::indexer::book::unlisted_orders(g.conn()).unwrap())
    };
    assert!(listed.is_empty(), "never offered to the matcher");
    let o = unlisted.iter().find(|o| o.id() == cov.0).expect("offered to the keepers");
    assert!(o.custody_ok(), "with its proven custody");
    let due = AnyState::KobAsk(a).refund_due(c.w.utxo(&create, 0).block_daa_score as i64).unwrap() as u64;
    assert_eq!(refund_job(o, due), Some((JobKind::Kill, due)));
}
