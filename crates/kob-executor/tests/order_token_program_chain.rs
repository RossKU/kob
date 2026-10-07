//! A wrong-length order placed through the chain: a consensus-valid genesis transaction (P2PK funding authorises the covenant
//! genesis, covenant id recomputed, placement record in the payload) whose order state pins a supported token program with
//! a prefix length that does not match it, mined into the mock node, followed by the real indexer (`Harness`), read back
//! through `book::listed_orders` (the matcher's own view) and matched by the real `engine::tick`. The indexer does not list
//! it, and the honest crossing next to it is matched every tick.

mod common;

use std::collections::BTreeSet;

use common::*;
use kaspa_consensus_core::hashing::covenant_id::covenant_id;
use kob_executor::indexer::book;
use kob_executor::matcher::book::{Clock, ListedOrder};
use kob_executor::matcher::engine::{tick, EngineConfig, TickInput};
use kob_executor::matcher::family::Families;
use kob_executor::matcher::wallet::LocalKeys;
use kob_executor::testkit::*;
use kob_protocol::build::*;
use kob_protocol::payload::{self, Record};
use kob_protocol::state::*;
use kob_protocol::tx::*;

/// A create of `odd` built by hand: the production builder refuses the state, so the create of a twin state is built, the
/// order output re-pointed at the odd state's script, the genesis covenant id recomputed and the placement record replaced;
/// signed and validated by the script engine (consensus-valid).
fn create_by_hand(w: &World, honest: AnyState, odd: AnyState, value: u64, maker: u8) -> SignedTx {
    assert!(build(&Action::CreateOrder(w.create(odd.clone(), value, maker, 0))).is_err(), "the builder refuses the state");
    let mut built = build(&Action::CreateOrder(w.create(honest, value, maker, 0))).expect("honest create");
    let oi = built.tx.outputs.iter().position(|o| o.covenant.is_some()).expect("order output");
    built.tx.outputs[oi].script_public_key = spk_to_string(&odd.spk());
    let auth = built.tx.outputs[oi].covenant.as_ref().unwrap().authorizing_input as usize;
    let (tx, _) = built.tx.to_tx().unwrap();
    let mut o = tx.outputs[oi].clone();
    o.covenant = None;
    let id = covenant_id(tx.inputs[auth].previous_outpoint, [(oi as u32, &o)].into_iter());
    built.tx.outputs[oi].covenant.as_mut().unwrap().covenant_id = id.as_bytes();
    built.tx.payload = payload::encode(&[Record::order(oi as u16, &odd, None, None)]).expect("placement record encodes");
    w.resign(built, true)
}

fn run_tick(orders: Vec<ListedOrder>, daa: u64) -> kob_executor::matcher::engine::TickReport {
    let inp = TickInput {
        orders,
        clock: Clock { daa, utc: 1_790_694_000 },
        funding: vec![KeyUtxo {
            utxo: Utxo { transaction_id: [0xfa; 32], index: 0, amount: 50 * KAS, block_daa_score: 500, covenant_id: None },
            pubkey: pk(MATCHER),
        }],
        excluded: BTreeSet::new(),
    };
    let signer = LocalKeys { operator: pk(MATCHER), keys: (1..=40u8).map(|n| (pk(n), sk(n))).collect() };
    tick(&inp, &EngineConfig::default(), &Families::default(), &signer)
}

#[tokio::test]
async fn a_chain_placed_wrong_length_bid_is_not_listed_and_does_not_stall_the_matcher() {
    let c = Ctx::new();
    // honest crossing: ask 10 whole at 2.50, bid at 2.60 funded for 5 whole
    let a = ask(MAKER_A, P250);
    let ask_tx = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, a.amount_left);
    let hb = bid(MAKER_B, P260);
    let bid_tx = c.w.create_tx(AnyState::KobBid(hb.clone()), hb.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&ask_tx, &bid_tx]).await;

    let listed0 = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    assert_eq!(listed0.len(), 2);
    let r0 = run_tick(listed0, c.daa() + 5);
    assert_eq!(r0.prepared.len(), 1, "control: the honest pair matches ({:?})", r0.skipped);

    // a bid at the best price (2.70), template prefix length off by one; consensus-valid genesis
    let other = 9u8;
    let twin = BidState { maker: pk(other), price: 270_000_000, ..bid(other, P260) };
    let mut bad = twin.clone();
    bad.tpl_prefix_len += 1;
    let value = twin.escrow(5 * WHOLE, 2).unwrap() as u64;
    let otx = create_by_hand(&c.w, AnyState::KobBid(twin), AnyState::KobBid(bad), value, other);
    c.push(&[&otx]).await;

    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    assert_eq!(listed.len(), 2, "the wrong-length bid is indexed, not listed");
    assert!(listed.iter().all(|o| o.order.state.token_tpl_lens().0 == a.tpl_prefix_len));
    let reason: Option<String> =
        c.hs.ingest
            .lock()
            .unwrap()
            .conn()
            .query_row("SELECT unlisted_reason FROM orders WHERE listed = 0", [], |r| r.get(0))
            .expect("one unlisted order");
    assert_eq!(reason.as_deref(), Some("bad_state:tokenProgram:not_the_pinned_program"));
    for t in 0..3 {
        let r = run_tick(listed.clone(), c.daa() + 5 + t);
        assert_eq!(r.prepared.len(), 1, "tick {t}: the honest crossing is matched ({:?})", r.skipped);
    }
}
