//! The published public-mint build of the KCC-20 reference (`KCC20PublicMint`: the `KCC20` actor of upstream's
//! `KCC20PublicMint` app, its holders' state opening with the context `gen__kcc20_template`) through the chain: an ask and a
//! bid on such a token are created, indexed, listed for the matcher (whose tick plans their crossing) and filled; the
//! indexer proves the token outputs of the fill under the program and records the custody chain. Nothing in the executor
//! names the program: it is one more embedded template (its KCC-1 actor-type handle) with its record-log code.

mod common;

use std::collections::BTreeSet;

use common::*;
use kob_executor::indexer::book;
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

const PM: TemplateId = TemplateId::Kcc20PublicMint;

fn on_pm_ask(a: AskState) -> AskState {
    let (h, p, s) = tok_fields(PM);
    AskState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(PM), ..a }
}
fn on_pm_bid(b: BidState) -> BidState {
    let (h, p, s) = tok_fields(PM);
    BidState { token_tpl_hash: h, tpl_prefix_len: p, tpl_suffix_len: s, refund_tip: rtip(PM), ..b }
}

#[test]
fn the_program_has_its_record_code_and_its_own_slot_limits() {
    assert_eq!(program_code(PM), Some(9));
    assert_eq!(program_from_code(9), Some(PM));
    let l = token_limits(kob_executor::matcher::family::Family::Kcc20, &token_template(PM).hash).unwrap();
    assert_eq!((l.max_in, l.max_out), (3, 3));
    assert_eq!(tok_fields(PM).1, 34, "the actor-type handle: the context push ends the template prefix");
}

#[tokio::test]
async fn ask_and_bid_on_a_published_build_token_are_indexed_listed_matched_and_filled() {
    let c = Ctx::new();
    let a = on_pm_ask(ask(MAKER_A, P250));
    let create = c.w.create_tx(AnyState::KobAsk(a.clone()), CARRIER, MAKER_A, 10 * WHOLE);
    let hb = on_pm_bid(bid(MAKER_B, P260));
    let bid_tx = c.w.create_tx(AnyState::KobBid(hb.clone()), hb.escrow(5 * WHOLE, 2).unwrap() as u64, MAKER_B, 0);
    c.push(&[&create, &bid_tx]).await;
    let cov = c.w.cov(&create, 0);
    assert_eq!(c.hs.status(&cov), "open");
    assert_eq!(c.hs.tip_state(&cov), Some(AnyState::KobAsk(a.clone())));
    // the custody is a holder of the published app: its script is the program's handle instance
    let custody_state = Kcc20State::custody(10 * WHOLE, cov.0, EXT);
    let custody = c.w.token_at(&create, 1, custody_state.clone());
    assert_eq!(spk_to_string(&custody_state.spk_with(template(PM))), create.tx.outputs[1].script_public_key);

    // the matcher's view: both orders listed, their crossing planned and validated
    let listed = { book::listed_orders(c.hs.ingest.lock().unwrap().conn()).unwrap() };
    assert_eq!(listed.len(), 2, "both orders on the published build are listed");
    assert!(listed.iter().all(|o| o.order.state.token_tpl_hash() == Some(token_template(PM).hash)));
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
    let rest = spk_to_string(&Kcc20State::custody(6 * WHOLE, cov.0, EXT).spk_with(template(PM)));
    let ci = fill.tx.outputs.iter().position(|o| o.script_public_key == rest).expect("custody after the fill");
    let live: i64 = c.hs.query("SELECT COUNT(*) FROM token_utxos WHERE owner = ?1 AND spent_block IS NULL", [&cov.0[..]]);
    assert_eq!(live, 1);
    let program: String =
        c.hs.query("SELECT program FROM token_holdings WHERE txid = ?1 AND idx = ?2", rusqlite::params![&fill.tx.id[..], ci as i64]);
    assert_eq!(program, "KCC20PublicMint", "the rest custody is proven under the published build");
    let pm_rows: i64 = c.hs.query("SELECT COUNT(*) FROM token_holdings WHERE program = ?1", ["KCC20PublicMint"]);
    assert!(pm_rows >= 2, "the rest custody and the taker's delivery ({pm_rows})");
}
