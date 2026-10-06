//! Strays (token UTXOs owned by an order id outside the protocol), cancel-replace, and hostile or
//! irrelevant payloads.

mod common;

use common::*;
use kob_executor::hex::{Hash32, HexBytes};
use kob_executor::indexer::reads;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::*;
use kob_protocol::tx::*;

fn cancel(c: &Ctx, order: OrderUtxo<AnyState>, custody: Option<TokenUtxo>, strays: Vec<TokenUtxo>) -> SignedTx {
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

fn ctx_of(c: &Ctx) -> reads::ReadCtx {
    reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None }
}

#[tokio::test]
async fn stray_is_flagged_never_counted_as_custody_and_swept_by_the_cancel() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);

    // somebody sends tokens to the order's id: exactly the custody amount, to be nasty
    let stray = c.w.stray(cov, 10 * WHOLE, TAKER);
    c.push(&[&stray]).await;
    let roles: Vec<(String, i64)> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT role, amount FROM token_utxos WHERE owner = ?1 ORDER BY role").unwrap();
        st.query_map([&cov.0[..]], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(roles, [("custody".to_string(), 10 * WHOLE), ("stray".to_string(), 10 * WHOLE)]);
    let v = view(&c, &cov);
    assert!(v.custody.as_ref().unwrap().ok, "one live custody of the exact amount: the stray does not count");
    let strays = v.strays.clone().unwrap();
    assert_eq!(strays.len(), 1);
    assert_eq!((strays[0].role.as_str(), strays[0].amount.as_str()), ("stray", "10000"));
    assert_eq!(strays[0].txid, hex(&stray.tx.id));
    // the ask is still liquidity, once
    {
        let g = c.hs.ingest.lock().unwrap();
        let bk = reads::book(g.conn(), &ctx_of(&c), &Hash32(TOKEN_COV), 10, true).unwrap();
        let reads::BookSide::Levels(asks) = bk.asks else { panic!() };
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].amount, (10 * WHOLE).to_string(), "a stray adds no liquidity");
        let list = reads::strays(g.conn(), &ctx_of(&c), None, 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].utxo.owner, cov.to_hex());
        assert!(!list[0].lost);
        assert_eq!(list[0].order_status.as_deref(), Some("open"));
    }

    // a fill is unaffected (the stray is not in the transaction)
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    assert!(view(&c, &cov).custody.unwrap().ok);
    assert_eq!(view(&c, &cov).strays.unwrap().len(), 1);

    // the maker's cancel is the only path that moves it: it sweeps custody and stray together
    let a6 = AskState { amount_left: 6 * WHOLE, ..a };
    let oi = find_cov(&fill, &cov).unwrap();
    let custody = c.w.token_at(&fill, find_custody(&fill, &cov, 6 * WHOLE).unwrap(), Kcc20State::custody(6 * WHOLE, cov.0, EXT));
    let stray_utxo =
        c.w.token_at(&stray, find_custody(&stray, &cov, 10 * WHOLE).unwrap(), Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let sweep = cancel(&c, c.w.order(&fill, oi, AnyState::KobAsk(a6)), Some(custody), vec![stray_utxo]);
    c.push(&[&sweep]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE spent_block IS NULL", []), 0);
    let g = c.hs.ingest.lock().unwrap();
    assert!(reads::strays(g.conn(), &ctx_of(&c), None, 10).unwrap().is_empty(), "swept");
}

fn hex(b: &[u8]) -> String {
    kob_executor::hex::encode(b)
}

#[tokio::test]
async fn every_token_owned_by_a_bid_is_a_stray_and_strays_to_dead_orders_are_lost() {
    let c = Ctx::new();
    let b0 = bid(MAKER_B, P245);
    let create = c.w.create_tx(AnyState::KobBid(b0.clone()), b0.escrow(4 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let stray = c.w.stray(cov, 5, TAKER);
    c.push(&[&stray]).await;
    let v = view(&c, &cov);
    assert!(v.custody.is_none(), "a bid holds KAS, not tokens");
    assert_eq!(v.strays.as_ref().unwrap().len(), 1);

    // cancel the bid, sweeping the stray; then a second stray arrives after the order is gone
    let stray_utxo = c.w.token_at(&stray, find_custody(&stray, &cov, 5).unwrap(), Kcc20State::custody(5, cov.0, EXT));
    let sweep = cancel(&c, c.w.order(&create, 0, AnyState::KobBid(b0)), None, vec![stray_utxo]);
    c.push(&[&sweep]).await;
    assert_eq!(c.hs.status(&cov), "cancelled");
    let late = c.w.stray(cov, 3, TAKER);
    c.push(&[&late]).await;
    let g = c.hs.ingest.lock().unwrap();
    let list = reads::strays(g.conn(), &ctx_of(&c), None, 10).unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].lost, "the order has terminated: the stray can no longer be swept");
    assert_eq!(list[0].order_status.as_deref(), Some("cancelled"));
}

#[tokio::test]
async fn tokens_sent_to_an_unknown_covenant_id_are_ignored() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let before = c.hs.snapshot();
    let stray = c.w.stray(Hash32([0x99; 32]), 7, TAKER);
    c.push(&[&stray]).await;
    // an owner that is no KOB order leaves no trace in the order tables; the outputs are only holdings of an allowlisted token
    let without_holdings = |snap: String| {
        snap.lines().filter(|l| !l.starts_with("holding ")).collect::<Vec<_>>().join(
            "
",
        )
    };
    assert_eq!(without_holdings(c.hs.snapshot()), without_holdings(before));
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1", [&[0x99u8; 32][..]]), 0);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_holdings WHERE role = 'owned' AND owner = ?1", [&[0x99u8; 32][..]]), 1);
}

#[tokio::test]
async fn cancel_replace_closes_the_old_order_and_creates_the_new_one_from_its_placement_record() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let old = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, old.0, EXT));
    let new_state = AskState { price: 240_000_000, ..a.clone() };
    let req = CancelOrder {
        prefund: None,
        order: c.w.order(&create, 0, AnyState::KobAsk(a)),
        custody: Some(custody),
        foreign: vec![],
        strays: vec![],
        tokens: vec![],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        replace: Some(Replacement {
            order: AnyState::KobAsk(new_state.clone()),
            value: CARRIER - 5_000_000,
            token_carrier: None,
            deadline: None,
        }),
        lock_time: 0,
        records: vec![note_record()],
        fee: FeeOptions::default(),
    };
    let amend = c.w.sign(&Action::CancelOrder(req));
    c.push(&[&amend]).await;
    let new = c.w.cov(&amend, 0);
    assert_ne!(new, old);
    assert_eq!(c.hs.status(&old), "cancelled");
    assert_eq!(c.hs.status(&new), "open");
    assert_eq!(c.hs.tip_state(&new), Some(AnyState::KobAsk(new_state)));
    assert!(view(&c, &new).custody.unwrap().ok, "the replacement holds the old custody's amount");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NOT NULL", [&old.0[..]]), 1);
}

// ---------------------------------------------------------------------------------------------
// hostile and irrelevant payloads

fn rewire(t: &SignedTx, f: impl FnOnce(&mut kob_executor::rpc::types::Tx)) -> kob_executor::rpc::types::Tx {
    let mut tx = wire_tx(&t.tx);
    f(&mut tx);
    tx
}

#[tokio::test]
async fn a_forged_placement_record_is_rejected_and_a_missing_one_hides_the_order() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    // 1. the record claims another price: the P2SH no longer matches
    let mut p = payload::decode(&create.tx.payload).unwrap().unwrap();
    if let Record::Order { state, template, .. } = &mut p.records[0] {
        let mut s = AnyState::decode(*template, state).unwrap();
        if let AnyState::KobAsk(x) = &mut s {
            x.price -= 1;
        }
        *state = s.encode();
    }
    let forged = rewire(&create, |t| t.payload = HexBytes(payload::encode(&p.records).unwrap()));
    // 2. no record at all
    let create2 = c.w.create_tx(AnyState::KobAsk(AskState { price: P250 + 1, ..a.clone() }), CARRIER, MAKER_A, 10 * WHOLE);
    let hidden = rewire(&create2, |t| t.payload = HexBytes(vec![]));
    // 3. garbage after the magic
    let create3 = c.w.create_tx(AnyState::KobAsk(AskState { price: P250 + 2, ..a.clone() }), CARRIER, MAKER_A, 10 * WHOLE);
    let garbage = rewire(&create3, |t| t.payload = HexBytes(b"KOB1\x02\x01\xff\xffzz".to_vec()));
    // 4. an x402-only KOB1 payload is fine and creates nothing
    let x402 = rewire(&create3, |t| {
        t.verbose_data.transaction_id = h("x402", 1);
        t.payload = HexBytes(payload::encode(&[Record::X402 { reference: vec![7; 32] }]).unwrap());
        t.outputs.retain(|o| o.covenant.is_none());
        t.inputs.iter_mut().for_each(|i| i.verbose_data = None);
    });
    c.hs.node.push_block(vec![forged, hidden, garbage, x402]);
    c.hs.sync().await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    let reasons: Vec<String> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT reason FROM rejects ORDER BY id").unwrap();
        st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(reasons[0].starts_with("placement:"), "{reasons:?}");
    assert!(reasons[1].starts_with("payload:"), "{reasons:?}");
}

/// A placement whose genesis group holds a second output next to the order (a sibling that
/// would carry the order's covenant id and could unlock its custody) is rejected and never listed.
#[tokio::test]
async fn a_placement_with_a_sibling_in_its_genesis_group_is_rejected() {
    use kaspa_consensus_core::hashing::covenant_id::covenant_id;
    use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint, TransactionOutput};
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    let rec = payload::recover_orders(&create.tx).unwrap();
    let (o, old) = (rec[0].output as usize, rec[0].covenant_id);
    let mut signed = create.clone();
    let tx = &mut signed.tx;
    let binding = tx.outputs[o].covenant.clone().unwrap();
    tx.outputs.push(TxOutputJson {
        value: 50_000_000,
        script_public_key: tx.outputs[o].script_public_key.clone(),
        covenant: Some(binding.clone()),
    });
    let inp = &tx.inputs[binding.authorizing_input as usize];
    let op = TransactionOutpoint { transaction_id: TransactionId::from_bytes(inp.transaction_id), index: inp.index };
    let group: Vec<(u32, TransactionOutput)> = tx
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, x)| x.covenant.as_ref() == Some(&binding))
        .map(|(k, x)| {
            (
                k as u32,
                TransactionOutput {
                    value: x.value,
                    script_public_key: spk_from_string(&x.script_public_key).unwrap(),
                    covenant: None,
                },
            )
        })
        .collect();
    let new = covenant_id(op, group.iter().map(|(k, x)| (*k, x))).as_bytes();
    for x in tx.outputs.iter_mut() {
        if let Some(b) = x.covenant.as_mut().filter(|b| b.covenant_id == old) {
            b.covenant_id = new;
        }
    }
    let cu = rec[0].custody.as_ref().unwrap();
    let tt = kob_protocol::artifacts::token_template_by_hash(&rec[0].order.token_tpl_hash().unwrap()).unwrap();
    let st = TokenState::custody(rec[0].order.family(), cu.state.amount(), new, cu.state.extension());
    tx.outputs[cu.output as usize].script_public_key = spk_to_string(&st.spk_with(tt));
    c.hs.node.push_block(vec![rewire(&signed, |_| {})]);
    c.hs.sync().await;
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 0);
    let reasons: Vec<String> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT reason FROM rejects ORDER BY id").unwrap();
        st.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].starts_with("placement:") && reasons[0].contains("genesis group has other outputs"), "{reasons:?}");
}

#[tokio::test]
async fn unrelated_transactions_and_other_covenants_leave_no_trace() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a), CARRIER, MAKER_A, 10 * WHOLE);
    c.w.include_with(&c.hs.node, &[&create], vec![noise_tx(1, false), noise_tx(2, true)]);
    c.hs.sync().await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM orders", []), 1);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM order_utxos", []), 1);
    assert_eq!(c.hs.query::<i64>("SELECT COUNT(*) FROM rejects", []), 0);
}

#[tokio::test]
async fn foreign_strays_are_flagged_and_the_maker_sweeps_every_stray_in_place() {
    use kob_protocol::family::Family;
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    // a stray of the order's own token and a FOREIGN stray (a KRON token no order trades) sent to the order id
    let own = c.w.stray(cov, 3 * WHOLE, TAKER);
    let kron = c.w.stray_for(Family::Kron, cov, 40, TAKER);
    c.push(&[&own, &kron]).await;
    let v = view(&c, &cov);
    let strays = v.strays.clone().unwrap();
    assert_eq!(strays.len(), 2, "{strays:?}");
    let f: Vec<_> = strays.iter().filter(|s| s.foreign).collect();
    assert_eq!(f.len(), 1, "the foreign stray is flagged (matcher.md 1.2)");
    assert_eq!(f[0].token, hex(&TOKEN_COV_KRON));
    assert!(f[0].state.is_some(), "its proven state is served (a client needs it to move it)");
    assert_eq!(f[0].program.as_deref(), Some(K3.name()), "and the program it was proven under");
    assert!(v.custody.as_ref().unwrap().ok, "neither stray is custody");
    {
        let g = c.hs.ingest.lock().unwrap();
        let listed = kob_executor::indexer::book::listed_orders(g.conn()).unwrap();
        let o = listed.iter().find(|o| o.id() == cov.0).expect("listed");
        assert_eq!((o.strays.len(), o.foreign.len()), (1, 1), "the matcher's book separates the foreign stray");
    }

    // the maker sweeps both IN PLACE: the same order continues (same script, covenant id, custody), the strays come back
    let oi = find_custody(&own, &cov, 3 * WHOLE).unwrap();
    let ki = find_custody_for(Family::Kron, &kron, &cov, 40).unwrap();
    let req = SweepOrder {
        order: c.w.order(&create, 0, AnyState::KobAsk(a.clone())),
        strays: vec![c.w.token_at(&own, oi, Kcc20State::custody(3 * WHOLE, cov.0, EXT))],
        foreign: vec![ForeignStrays {
            token: TokenRef { covenant_id: TOKEN_COV_KRON, program: K3 },
            utxos: vec![c.w.token_at(&kron, ki, TokenState::custody(Family::Kron, 40, cov.0, [0; 32]))],
        }],
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        token_carrier: None,
        lock_time: 0,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let sweep = c.w.sign(&Action::SweepOrder(req));
    c.push(&[&sweep]).await;
    assert_eq!(c.hs.status(&cov), "open", "a sweep never ends the order");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a.clone())));
    let v = view(&c, &cov);
    assert_eq!(v.current.as_ref().unwrap().txid, hex(&sweep.tx.id));
    assert!(v.strays.unwrap().is_empty(), "both strays were swept");
    assert!(v.custody.unwrap().ok, "the custody stays where it was");
    {
        let g = c.hs.ingest.lock().unwrap();
        let kinds: Vec<String> = g
            .conn()
            .prepare("SELECT kind FROM order_events WHERE covenant_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([&cov.0[..]], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(kinds, ["create", "sweep"]);
        let listed = kob_executor::indexer::book::listed_orders(g.conn()).unwrap();
        assert!(listed.iter().any(|o| o.id() == cov.0), "still in the matcher's book");
    }
    // the order still fills from its untouched custody
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&sweep, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
}

#[tokio::test]
async fn a_sweep_record_on_anything_but_the_same_order_is_refused() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let own = c.w.stray(cov, 3 * WHOLE, TAKER);
    c.push(&[&own]).await;
    let oi = find_custody(&own, &cov, 3 * WHOLE).unwrap();
    // a cancel that continues the id under ANOTHER script (a cheaper price) while claiming a sweep: the record fails, the
    // continuation is unproven (state unknown, listed nowhere), as for any unproven continuation of a cancel
    let mut cheaper = a.clone();
    cheaper.price = P250 - 1_000_000;
    let req = AmendOrder {
        order: c.w.order(&create, 0, AnyState::KobAsk(a.clone())),
        amended: AnyState::KobAsk(cheaper),
        value: None,
        funding: vec![c.w.coin(MAKER_A, 10)],
        change: None,
        lock_time: 0,
        deadline: None,
        records: vec![],
        fee: FeeOptions::default(),
    };
    let mut built = build(&Action::AmendOrder(req)).unwrap();
    built.tx.payload = kob_protocol::payload::encode(&[Record::Sweep { output: 0, input: 0 }]).unwrap();
    let (tx, entries) = built.tx.to_tx().unwrap();
    for r in &mut built.sign {
        r.sighash = kob_protocol::tx::sighash(&tx, &entries, r.input_index);
    }
    let forged = c.w.finish(&built);
    c.push(&[&forged]).await;
    let v = view(&c, &cov);
    assert!(!v.state_known, "an unproven continuation");
    let rejects: Vec<String> = {
        let g = c.hs.ingest.lock().unwrap();
        let mut st = g.conn().prepare("SELECT reason FROM rejects").unwrap();
        let v: Vec<String> = st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        v
    };
    assert!(rejects.iter().any(|r| r.starts_with("sweep:")), "{rejects:?}");
    let _ = oi;
}
