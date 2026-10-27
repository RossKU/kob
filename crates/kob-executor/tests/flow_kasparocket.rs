//! KaspaRocket's token program (`KaspaRocketToken11458`: the `KCC20` actor of upstream's `KCC20PublicMint` app built with 10
//! token inputs / 10 token outputs, an 11,458-byte redeem script per token input) through the chain: an ask and a bid on such a
//! token are created, indexed, listed for the matcher (whose tick plans their crossing) and filled; the indexer proves the
//! token outputs of the fill under the program and records the custody chain; and a plain transfer at the program's full
//! 10 / 10 (ten 11.5 KB token inputs, ten outputs) is followed: every output is proven under the program and the inputs are
//! spent. Nothing in the executor names the program: it is one more embedded template (its KCC-1 actor-type handle) with its
//! record-log code.

mod common;

use std::collections::BTreeSet;

use common::*;
use kob_executor::hex::Hash32;
use kob_executor::indexer::book;
use kob_executor::indexer::reads::{self, HoldingFilter, HoldingView};
use kob_executor::indexer::record::{program_code, program_from_code};
use kob_executor::matcher::book::Clock;
use kob_executor::matcher::engine::{tick, EngineConfig, TickInput};
use kob_executor::matcher::family::{token_limits, Families};
use kob_executor::matcher::wallet::LocalKeys;
use kob_executor::testkit::*;
use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::build::*;
use kob_protocol::state::*;
use kob_protocol::tx::*;

const KR: TemplateId = TemplateId::KaspaRocketToken11458;

fn on_kr_ask(a: AskState) -> AskState {
    let (h, p, s) = tok_fields(KR);
    AskState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(KR), ..a }
}
fn on_kr_bid(b: BidState) -> BidState {
    let (h, p, s) = tok_fields(KR);
    BidState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(KR), ..b }
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn holdings(c: &Ctx, f: &HoldingFilter) -> Vec<HoldingView> {
    let g = c.hs.ingest.lock().unwrap();
    let ctx = reads::ReadCtx { node_daa: Some(c.daa()), settle_depth_daa: 100, now_unix: None };
    reads::holdings(g.conn(), &ctx, f, 100, None).unwrap().items
}

#[test]
fn the_program_has_its_record_code_and_its_own_slot_limits() {
    assert_eq!(program_code(KR), Some(11));
    assert_eq!(program_from_code(11), Some(KR));
    let l = token_limits(kob_executor::matcher::family::Family::Kcc20, &token_template(KR).hash).unwrap();
    // the program takes 10 / 10; a transaction spending KOB orders carries at most 8 token inputs of a token (the orders' bound)
    assert_eq!((l.max_in, l.max_out), (8, 10));
    assert_eq!(
        tok_fields(KR),
        (token_template(KR).hash, 34, 11_312),
        "the actor-type handle: the context push ends the template prefix"
    );
}

#[tokio::test]
async fn ask_and_bid_on_a_kasparocket_token_are_indexed_listed_matched_and_filled() {
    let c = Ctx::new();
    let a = on_kr_ask(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let hb = on_kr_bid(bid(MAKER_B, P260));
    let bid_tx = c.w.create_tx(AnyState::KobBid(hb.clone()), hb.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create, &bid_tx]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a.clone())));
    // the custody is a holder of the program: its script is the program's handle instance
    let custody_state = Kcc20State::custody(10 * WHOLE, cov.0, EXT);
    let custody = c.w.token_at(&create, 1, custody_state.clone());
    assert_eq!(spk_to_string(&custody_state.spk_with(template(KR))), create.tx.outputs[1].script_public_key);

    // the matcher's view: both orders listed, their crossing planned and validated
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    assert_eq!(listed.len(), 2, "both orders on the program are listed");
    assert!(listed.iter().all(|o| o.order.state.token_tpl_hash() == Some(token_template(KR).hash)));
    let inp = TickInput {
        orders: listed,
        clock: Clock { daa: c.daa() + 5, utc: 1_790_694_000 },
        funding: vec![KeyUtxo {
            utxo: Utxo { transaction_id: [0xfa; 32], index: 0, amount: 50 * KAS, block_daa_score: 500, covenant_id: None },
            pubkey: pk(MATCHER),
        }],
        excluded: BTreeSet::new(),
    };
    let signer = LocalKeys { operator: pk(MATCHER), keys: (1..=40u8).map(|n| (pk(n), sk(n))).collect() };
    let r = tick(&inp, &EngineConfig::default(), &Families::default(), &signer);
    assert!(r.anomalies.is_empty(), "{:?}", r.anomalies);
    assert_eq!(r.prepared.len(), 1, "the crossing is matched ({:?})", r.skipped);
    assert!(r.prepared[0].validation.is_some(), "the matcher's transaction passed the engine");

    // a taker buys 4 whole tokens: the indexer follows the fill and proves the token outputs under the program
    let b = batch(&c.w, c.daa(), vec![Leg::Ask { order: c.w.order(&create, 0, a.clone()), custody, amount: 4 * WHOLE, t: None }]);
    let fill = c.w.sign(&Action::Batch(b));
    c.push(&[&fill]).await;
    assert_eq!(c.hs.status(&cov), "partial");
    let a6 = AskState { amount_left: 6 * WHOLE, ..a.clone() };
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a6)));
    let rest = spk_to_string(&Kcc20State::custody(6 * WHOLE, cov.0, EXT).spk_with(template(KR)));
    let ci = fill.tx.outputs.iter().position(|o| o.script_public_key == rest).expect("custody after the fill");
    let live: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]);
    assert_eq!(live, 1);
    let program: String =
        c.hs.query("SELECT program FROM token_holdings WHERE txid = ?1 AND idx = ?2", rusqlite::params![&fill.tx.id[..], ci as i64]);
    assert_eq!(program, "KaspaRocketToken11458", "the rest custody is proven under the program");
    let rows: i64 = c.hs.query("SELECT COUNT(*) FROM token_holdings WHERE program = ?1", ["KaspaRocketToken11458"]);
    assert!(rows >= 2, "the rest custody and the taker's delivery ({rows})");
}

/// The program's full transfer: ten token inputs (a leader and nine delegators, each revealing the 11,458-byte script) and ten
/// outputs, built by the real `SendTokens` builder and validated in the engine; the indexer proves all ten outputs under the
/// program (the leader's next states parsed from its witness) and marks every input spent.
#[tokio::test]
async fn a_ten_by_ten_transfer_is_followed_output_by_output() {
    let c = Ctx::new();
    let inputs: Vec<TokenUtxo> = (0..10).map(|_| c.w.token(TAKER, 100)).collect();
    let req = SendTokens {
        token: TokenRef { covenant_id: TOKEN_COV, program: KR },
        tokens: inputs,
        recipients: (0..9u8).map(|i| TokenRecipient { pubkey: pk(20 + i), amount: 100 + i64::from(i), carrier: CARRIER }).collect(),
        token_change: Some(pk(TAKER)),
        token_change_carrier: CARRIER,
        funding: vec![c.w.coin(TAKER, 100)],
        records: vec![],
        change: None,
        fee: FeeOptions::default(),
    };
    let tx = c.w.sign(&Action::SendTokens(req));
    let token_inputs = tx.tx.inputs.iter().filter(|i| i.signature_script.len() > 11_458).count();
    assert_eq!(token_inputs, 10, "ten token inputs, each revealing the program");
    c.push(&[&tx]).await;
    let all = holdings(&c, &HoldingFilter { owner: None, token: Some(Hash32(TOKEN_COV)), include_spent: false });
    assert_eq!(all.len(), 10, "{all:?}");
    assert!(all.iter().all(|v| v.program == "KaspaRocketToken11458" && v.txid == hex_of(&tx.tx.id)), "{all:?}");
    let mut amounts: Vec<i64> = all.iter().map(|v| v.amount.parse().unwrap()).collect();
    amounts.sort();
    // 100..=108 to the nine recipients, the change 1000 - 936 = 64 back to the sender
    assert_eq!(amounts, [64, 100, 101, 102, 103, 104, 105, 106, 107, 108]);
    let change = holdings(&c, &HoldingFilter { owner: Some(pk(TAKER).to_vec()), token: None, include_spent: false });
    assert_eq!((change.len(), change[0].amount.as_str()), (1, "64"));
}
