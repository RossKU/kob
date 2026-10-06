//! Regression: a token output owned by an order id is that order's custody only if it is exactly the
//! custody the covenant rules require; any other same-owner output is a stray.
//!
//! The ask covenants only guard token INPUTS (`noStrays`), never outputs, so a taker who fills an order can add an extra token
//! output owned by the order id (the engine accepts it, `World::resign(.., true)` runs the whole transaction). The indexer used
//! to store every such output as `custody`: the ask then had two custody rows and was dropped from the matcher's book, a bid that
//! owns no tokens was rejected by `custody_ok`, and an if-done exit was unlisted so its stop never fired. Now the custody is the
//! one output of exactly `amountLeft`, plain, of the order's own token; everything else is a stray (listed, shown
//! by `/v1/strays`, never liquidity).

mod common;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book;
use kob_executor::indexer::reads::{orders, strays, OrderFilter, ReadCtx};
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// Patches the built fill: takes `1` unit off the taker's net token output (`net_before` units) and hands it to `victim` as an extra
/// token output of the same program, pays the extra carrier from the change, then re-signs and runs the WHOLE transaction in the
/// script engine (it panics if the engine refuses it: the transaction is consensus-valid).
fn poison(c: &Ctx, mut built: BuiltTx, net_before: i64, victim: Hash32) -> SignedTx {
    let taker_net_before = Kcc20State::p2pk(net_before, pk(TAKER), EXT);
    let taker_net_after = Kcc20State::p2pk(net_before - 1, pk(TAKER), EXT);
    let extra = Kcc20State::custody(1, victim.0, EXT);
    let mut program = None;
    let mut leader_input = 0usize;
    for (pi, p) in built.plans.iter_mut().enumerate() {
        if let SigPlan::TokenLeader { template, next_states, .. } = p {
            program = Some(*template);
            leader_input = pi;
            let k = next_states.iter().position(|s| *s == taker_net_before).expect("taker net output is authorised");
            next_states[k] = taker_net_after.clone();
            next_states.push(extra.clone());
        }
    }
    let tpl = kob_protocol::artifacts::token_template(program.expect("a token leader"));
    let before_spk = spk_to_string(&TokenState::Kcc20(taker_net_before).spk_with(tpl));
    let after_spk = spk_to_string(&TokenState::Kcc20(taker_net_after).spk_with(tpl));
    let extra_spk = spk_to_string(&TokenState::Kcc20(extra).spk_with(tpl));
    let ti = built.tx.outputs.iter().position(|o| o.script_public_key == before_spk).expect("taker output");
    built.tx.outputs[ti].script_public_key = after_spk;
    let mut extra_out = built.tx.outputs[ti].clone();
    extra_out.script_public_key = extra_spk;
    built.tx.outputs.push(extra_out.clone());
    let ci = built.fee.change_output.expect("change") as usize;
    built.tx.outputs[ci].value -= extra_out.value + 5_000_000;
    built.tx.inputs[leader_input].compute_budget += 60;
    let (tx, entries) = built.tx.to_tx().unwrap();
    tx.set_storage_mass(masses(&tx, &entries).storage);
    built.tx = TxJson::from_tx(&tx, &entries);
    c.w.resign(built, true)
}

fn live_roles(c: &Ctx, owner: &Hash32) -> Vec<(String, i64)> {
    let g = c.hs.ingest.lock().unwrap();
    let mut st =
        g.conn().prepare("SELECT role, amount FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL ORDER BY amount").unwrap();
    st.query_map([&owner.0[..]], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
}

fn stray_count(c: &Ctx) -> usize {
    let g = c.hs.ingest.lock().unwrap();
    strays(g.conn(), &ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }, None, 10).unwrap().len()
}

#[tokio::test]
async fn a_hostile_taker_cannot_delist_an_ask_with_an_extra_token_output() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));

    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let mut b = batch(&c.w, c.daa(), vec![leg]);
    b.taker_tokens = vec![c.w.token(TAKER, 5)];
    let built = build(&Action::Batch(b)).expect("honest fill");
    let hostile = poison(&c, built, 4 * WHOLE + 5, cov);
    c.push(&[&hostile]).await;

    // the 6 whole tokens left are the custody, the one extra unit is a stray
    assert_eq!(live_roles(&c, &cov), [("stray".to_string(), 1), ("custody".to_string(), 6 * WHOLE)]);
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    let o = listed.iter().find(|o| o.id() == cov.0).expect("the ask is still in the matcher's book");
    assert!(o.custody_ok(), "custody exact: 6 whole tokens");
    assert_eq!(o.strays.len(), 1, "the extra unit is reported as a stray and never liquidity");
    assert_eq!(stray_count(&c), 1, "the wallet is told to sweep it");
    let v = view(&c, &cov).custody.expect("ask custody view");
    assert!(v.ok, "{v:?}");
    // the maker's list (the wallet's My orders) carries the same custody and strays for the live order (W-12); a list
    // without a maker filter stays lean
    let g = c.hs.ingest.lock().unwrap();
    let rc = ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    let mine = OrderFilter { maker: Some(pk(MAKER_A).to_vec()), ..Default::default() };
    let page = orders(g.conn(), &rc, &mine, 50, None).unwrap();
    let row = page.items.iter().find(|o| o.covenant_id == cov.to_hex()).expect("listed for its maker");
    assert!(row.custody.as_ref().is_some_and(|k| k.ok), "{:?}", row.custody);
    assert_eq!(row.strays.as_ref().map(|s| s.len()), Some(1));
    let all = orders(g.conn(), &rc, &OrderFilter::default(), 50, None).unwrap();
    let row = all.items.iter().find(|o| o.covenant_id == cov.to_hex()).unwrap();
    assert!(row.custody.is_none() && row.strays.is_none());
}

#[tokio::test]
async fn a_hostile_taker_cannot_break_a_bid_with_a_token_output_to_its_id() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P260);
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let mut b = batch(&c.w, c.daa(), vec![Leg::Bid { order: c.w.order(&create, 0, b0.clone()), amount: 4 * WHOLE, t: None }]);
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE + 5)];
    let built = build(&Action::Batch(b)).expect("honest fill");
    let hostile = poison(&c, built, 5, cov);
    c.push(&[&hostile]).await;

    // a bid holds KAS, never tokens: every token owned by its id is a stray
    assert_eq!(live_roles(&c, &cov), [("stray".to_string(), 1)]);
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    let o = listed.iter().find(|o| o.id() == cov.0).expect("the bid is still listed");
    assert!(o.custody.is_none() && o.custody_ok(), "no custody, and the bid stays matchable");
    assert_eq!(stray_count(&c), 1);
}

#[tokio::test]
async fn a_hostile_taker_cannot_unlist_an_if_done_exit_with_an_extra_output() {
    let c = Ctx::new();
    let ib = IfdBidState { min_fill: 3 * WHOLE, ..ifd_bid(MAKER_A, 10) };
    let create = c.w.create_tx(AnyState::KobIfdBid(ib.clone()), ib.escrow().unwrap() as u64, MAKER_A, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let mut b = batch(
        &c.w,
        c.daa(),
        vec![Leg::IfdBid { order: c.w.order(&create, 0, ib.clone()), amount: 4 * WHOLE, evidence: None, t: None }],
    );
    b.taker_tokens = vec![c.w.token(TAKER, 4 * WHOLE + 5)];
    let built = build(&Action::Batch(b)).expect("honest fill");
    // the exit's covenant id is fixed by the entry input and the exit output; the extra token output does not change it
    let honest = c.w.resign(built.clone(), true);
    let (_, exit) = find_fresh(&honest, &[cov]).expect("the exit output");
    let hostile = poison(&c, built, 5, exit);
    let (_, exit2) = find_fresh(&hostile, &[cov]).expect("the exit output");
    assert_eq!(exit, exit2, "the exit id is the same in the hostile transaction");
    c.push(&[&hostile]).await;

    assert_eq!(live_roles(&c, &exit), [("stray".to_string(), 1), ("custody".to_string(), 4 * WHOLE)]);
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    let o = listed.iter().find(|o| o.id() == exit.0).expect("the exit (a stop-loss / take-profit) is still listed");
    assert!(o.custody_ok());
    assert_eq!(o.strays.len(), 1);
    assert_eq!(stray_count(&c), 1);
}
