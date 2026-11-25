//! The lane rule (R9-K, `docs/spec/indexer-lanes.md`): KOB state is created only by native-lane transactions, and a
//! transaction on any other lane (a KIP-21 user lane) can only end what the index tracks.
//!
//! - An order spent on a user lane is closed with the reason `spent_on_other_lane`, the spending transaction and what
//!   the spend left under the order's covenant id (its remainder, with the state the processor proves from its
//!   script). The maker takes the remainder back with `kobd db import-orders` (here `recover::import_orders`), or finds
//!   and cancels it with `kob recover --amount-left`.
//! - A holding spent on a user lane is marked spent by that transaction; tokens received there are not shown, and a
//!   later native transfer of them (to oneself) brings them back into view.

mod common;

use common::*;
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::family::Family;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kobd::hex::{Hash32, HexBytes};
use kobd::indexer::reads::{self, HoldingFilter, HoldingView};
use kobd::recover::{import_orders, ExportedOrder, MakerExport};
use kobd::rpc::types::{AddressUtxo, AddressUtxoEntry, Outpoint};
use kobd::script::{spk_address, spk_bytes};
use kobd::testkit::*;

/// The user lane every test transaction here goes on: namespace `02 00 00 07`.
const LANE: [u8; 4] = [2, 0, 0, 7];

/// Includes `t` in a block as a transaction on [`LANE`] and lets the indexer follow.
async fn push_on_lane(c: &Ctx, t: &SignedTx) {
    let mut w = wire_tx(&t.tx);
    let mut sub = [0u8; 20];
    sub[..4].copy_from_slice(&LANE);
    w.subnetwork_id = Some(HexBytes(sub.to_vec()));
    c.w.note_daa(&[t], c.hs.node.tip_daa() + 1);
    c.w.include_with(&c.hs.node, &[], vec![w]);
    c.hs.sync().await;
}

/// An unspent output as the node's `getUtxosByAddresses` reports it.
fn node_utxo(spk: &[u8], cov: Hash32, txid: Hash32, idx: u32, amount: u64) -> (String, AddressUtxo) {
    (
        spk_address(spk, "testnet-10").unwrap(),
        AddressUtxo {
            outpoint: Outpoint { transaction_id: txid, index: idx },
            utxo_entry: AddressUtxoEntry {
                amount,
                script_public_key: HexBytes(spk.to_vec()),
                block_daa_score: 1_050,
                covenant_id: Some(cov),
            },
        },
    )
}

/// A taker fills 4 of an ask's 10 whole tokens through a user-lane transaction: a real trade at the maker's price,
/// which anyone can make. The index closes the order (it creates no state from a user lane), with the reason, the
/// spending transaction and the remainder: 6 whole tokens under the same covenant id, the state proven from the
/// remainder's script. The maker's list shows it as well. With that state the maker imports the remainder, which is
/// then open again with its custody verified, so it can be cancelled from the view. The closed row makes nobody sign
/// anything: a closed order is in no book and no matcher plans it.
#[tokio::test]
async fn an_order_filled_on_a_user_lane_is_closed_with_its_remainder_which_its_maker_imports() {
    let c = Ctx::new();
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");

    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    push_on_lane(&c, &fill).await;

    assert_eq!(c.hs.status(&cov), "closed", "no state is followed past a user lane");
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    let oi = find_cov(&fill, &cov).unwrap();
    let v = view(&c, &cov);
    assert!(v.current.is_none() && v.custody.as_ref().is_none_or(|cv| cv.utxo.is_none()), "nothing live is shown");
    let end = v.lane_end.clone().expect("the lane end on the view");
    assert_eq!(end["reason"], "spent_on_other_lane");
    assert_eq!(end["lane"], "02000007");
    assert_eq!(end["txid"], Hash32(fill.tx.id).to_hex());
    let rem = &end["remainder"][0];
    assert_eq!(rem["index"], oi);
    assert_eq!(rem["amount_left"], (6 * WHOLE).to_string());
    let state_hex = rem["state"].as_str().expect("the remainder's state, proven from its script");
    assert_eq!(state_hex, kobd::hex::encode(&AnyState::KobAsk(a6.clone()).encode()));
    // the maker's list carries it too
    {
        let g = c.hs.ingest.lock().unwrap();
        let ctx = reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
        let f = reads::OrderFilter { maker: Some(pk(MAKER_A).to_vec()), ..Default::default() };
        let page = reads::orders(g.conn(), &ctx, &f, 50, None).unwrap();
        let mine = page.items.iter().find(|o| o.covenant_id == cov.to_hex()).expect("listed");
        assert_eq!(mine.lane_end.as_ref().map(|e| e["reason"].clone()), Some("spent_on_other_lane".into()));
    }

    // the maker takes it back: the remainder and its custody are on the node
    let ci = find_custody(&fill, &cov, 6 * WHOLE).unwrap();
    let order_spk = spk_bytes(&AnyState::KobAsk(a6.clone()).spk());
    let custody_spk = spk_bytes(&Kcc20State::custody(6 * WHOLE, cov.0, EXT).spk_with(template(T3)));
    c.hs.node.set_utxos(vec![
        node_utxo(&order_spk, cov, Hash32(fill.tx.id), oi as u32, fill.tx.outputs[oi].value),
        node_utxo(&custody_spk, Hash32(TOKEN_COV), Hash32(fill.tx.id), ci as u32, fill.tx.outputs[ci].value),
    ]);
    let export = MakerExport {
        version: 2,
        network: "testnet-10".into(),
        orders: vec![ExportedOrder {
            template_hash: Hash32(template(TemplateId::KobAsk).hash),
            state: HexBytes(kobd::hex::decode(state_hex).unwrap()),
            extension_commitment: Some(Hash32(EXT)),
            prefund_extension_commitment: None,
            covenant_id: Some(cov),
            genesis_txid: None,
            status: None,
            parent: None,
        }],
    };
    let rep = import_orders(&c.hs.ingest, c.hs.node.as_ref(), "testnet-10", &export).await.unwrap();
    assert_eq!(rep.imported, 1, "{rep:?}");
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a6)));
    let v = view(&c, &cov);
    assert!(v.current.is_some(), "a current outpoint to cancel");
    let cv = v.custody.expect("ask-side view");
    assert!(cv.ok && cv.utxo.is_some(), "custody verified against the node: {cv:?}");
}

fn holdings(c: &Ctx, key: u8, spent: bool) -> Vec<HoldingView> {
    let g = c.hs.ingest.lock().unwrap();
    let ctx = reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    let f = HoldingFilter { owner: Some(pk(key).to_vec()), token: None, include_spent: spent };
    reads::holdings(g.conn(), &ctx, &f, 100, None).unwrap().items
}

/// A transfer of `amount` from `input` (owned by `sender`) to `to`, change back to the sender; not included.
fn send_tx(c: &Ctx, input: TokenUtxo, sender: u8, to: u8, amount: i64) -> (SignedTx, TokenUtxo, TokenUtxo) {
    let total = input.state.amount();
    let req = SendTokens {
        token: TokenRef { covenant_id: TOKEN_COV, program: T3 },
        tokens: vec![input],
        recipients: vec![TokenRecipient { pubkey: pk(to), amount, carrier: CARRIER }],
        token_change: Some(pk(sender)),
        token_change_carrier: CARRIER,
        funding: vec![c.w.coin(sender, 100)],
        records: vec![],
        change: None,
        fee: FeeOptions::default(),
    };
    let tx = c.w.sign(&Action::SendTokens(req));
    // the DAA its block will have (the next one), for the UTXOs taken from it
    c.w.note_daa(&[&tx], c.hs.node.tip_daa() + 1);
    let tpl = token_template(T3);
    let at = |st: &TokenState| {
        let spk = spk_to_string(&st.spk_with(tpl));
        tx.tx.outputs.iter().position(|o| o.script_public_key == spk).expect("output of the state")
    };
    let got = TokenState::user(Family::Kcc20, amount, pk(to), EXT);
    let change = TokenState::user(Family::Kcc20, total - amount, pk(sender), EXT);
    let (gi, ci) = (at(&got), at(&change));
    let (g, ch) = (c.w.token_at(&tx, gi, got), c.w.token_at(&tx, ci, change));
    (tx, g, ch)
}

/// A holding spent on a user lane is marked spent by that transaction; what the transaction sends is not shown (not
/// the recipient's tokens, not the sender's change); a later native transfer of the received tokens to oneself shows
/// them again, proven from that native transaction.
#[tokio::test]
async fn a_holding_spent_on_a_user_lane_ends_and_tokens_received_there_return_with_a_native_transfer() {
    let c = Ctx::new();
    let tok = c.w.token_for(Family::Kcc20, TAKER, 1_000);
    let (tx1, got, _) = send_tx(&c, tok, TAKER, MAKER_A, 300);
    c.push(&[&tx1]).await;
    assert_eq!(holdings(&c, MAKER_A, false).len(), 1);

    let (tx2, to_b, _) = send_tx(&c, got, MAKER_A, MAKER_B, 100);
    push_on_lane(&c, &tx2).await;
    assert!(holdings(&c, MAKER_A, false).is_empty(), "the 300 are spent, the change of 200 is not shown");
    let spent = holdings(&c, MAKER_A, true);
    assert_eq!(spent.len(), 1);
    assert_eq!(spent[0].spent_txid.as_deref(), Some(Hash32(tx2.tx.id).to_hex().as_str()), "spent by the user-lane transaction");
    assert!(holdings(&c, MAKER_B, true).is_empty(), "tokens received on a user lane are not shown");

    // MAKER_B moves them to itself on the native lane: proven from that transaction, they are shown again
    let (tx3, _, _) = send_tx(&c, to_b, MAKER_B, MAKER_B, 40);
    c.push(&[&tx3]).await;
    let b: Vec<String> = holdings(&c, MAKER_B, false).iter().map(|v| v.amount.clone()).collect();
    assert_eq!(b.len(), 2, "{b:?}");
    assert!(b.contains(&"40".to_string()) && b.contains(&"60".to_string()), "{b:?}");
}

/// A store rebuilt from its record log under the lane rule comes out identical: the user-lane spend is logged as such
/// (record-log format 6, `TxRecord::lane`), so the replay closes the order again instead of reading the spend as a
/// native fill. (A log an earlier build wrote holds no lane: its records replay as they were read then.)
#[tokio::test]
async fn a_store_rebuilt_from_its_record_log_reads_a_user_lane_spend_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let (log, _) = kobd::indexer::recordlog::RecordLog::open(dir.path(), 1 << 20, 0).unwrap();
    let c = Ctx::with(Harness::new(conn(), Some(log)));
    let a = ask(MAKER_A, P250);
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    c.push(&[&create]).await;
    let cov = c.w.cov(&create, 0);
    let custody = c.w.token_at(&create, 1, Kcc20State::custody(10 * WHOLE, cov.0, EXT));
    let leg = Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None };
    let fill = c.w.sign(&Action::Batch(batch(&c.w, c.daa(), vec![leg])));
    push_on_lane(&c, &fill).await;
    assert_eq!(c.hs.status(&cov), "closed");
    let live = c.hs.snapshot();
    drop(c);
    let fresh = kobd::indexer::db::open_memory("testnet-10").unwrap();
    let mut ing = kobd::indexer::ingest::Ingest::new(fresh, processor(), None).with_config(wide_window());
    let logged = kobd::indexer::recordlog::read_all(dir.path()).unwrap();
    ing.init_cursor(&kobd::indexer::ingest::Cursor { hash: logged.records[0].1.start, daa: 0 }).unwrap();
    ing.replay(logged.records).unwrap();
    assert_eq!(snapshot(ing.conn()), live);
}
