//! Router intent seeds: every one of the 18 KCC-20 router actors (fill shapes, on the 8/8 program; the 3/3 and KRON
//! programs and the KRON actors run in `kob-protocol/tests/intent_builders.rs`) executed by a keeper against plain asks and bids
//! (no signature at all: every input is a covenant), and every intent kind expired by anyone. The fixtures mirror
//! `kob-protocol/tests/intent_builders.rs`; the intent UTXO and its lock carry a synthetic covenant id (the engine checks
//! the covenant context of the spending transaction, not the history of its inputs).

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{
    build_execute_intent, build_expire_intent, intent_budgets, ExecuteIntent, ExpireIntent, IntentAsk, IntentBid,
};
use kob_protocol::router::{Actor, IntentKind, IntentState, ACTORS};
use kob_protocol::state::*;
use kob_protocol::tx::{BuiltTx, TokenUtxo, Utxo};

use crate::common::*;

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const TOK_A: [u8; 32] = TOKEN_COV;
const TOK_B: [u8; 32] = TOKEN_B;
const PAYER: u8 = 11;
const KEEPER_K: u8 = 12;
const MCARRIER: u64 = KAS;
const FILLER: u64 = KAS / 5;
const DEADLINE: i64 = 1_800_000_000_000;
// distinct from every order covenant id of the fixtures (asks 0x5a..0x5c, bids 0x7e..0x80)
const INTENT_ID: [u8; 32] = [0x4f; 32];

/// An ask of `n` whole tokens of B of which the execution takes `take` whole tokens.
fn ask_leg(tag: u8, maker: u8, n: i64, take: i64, tif: i64) -> IntentAsk {
    let c = cov(0x50 + tag);
    let s = AskState { token_cov_id: TOK_B, tif, ..ask_n(maker, P250, P8, n) };
    IntentAsk {
        order: order(tag, CARRIER, c, 1_000, s),
        custody: tok_of(tag + 100, n * WHOLE, c, SCHEME_COVID, 1_000, TOK_B),
        amount: take * WHOLE,
    }
}

/// A bid that buys `n` whole tokens and then rests (`ends = false`, funded for more) or ends (funded for `n` whole
/// tokens and half a minimum fill: it cannot afford another one).
fn bid_leg(tag: u8, maker: u8, n: i64, ends: bool) -> IntentBid {
    let s = BidState { tif: 0, ..bid(maker, P245, P8) };
    let value =
        if ends { s.escrow(n * WHOLE, 1).unwrap() + s.used(WHOLE).unwrap() / 2 } else { s.escrow((n + 5) * WHOLE, 1).unwrap() };
    IntentBid { order: order(tag, value as u64, cov(0x60 + tag), 1_000, s), amount: n * WHOLE }
}

fn legs_for(actor: &Actor) -> (Vec<IntentAsk>, Vec<IntentBid>) {
    let s = actor.shape;
    let asks = (0..s.asks)
        .map(|i| {
            let last = i + 1 == s.asks;
            if last && s.last_ask_rests {
                ask_leg(10 + i as u8, 20 + i as u8, 10, 2, TIF_GTC)
            } else {
                ask_leg(10 + i as u8, 20 + i as u8, 2, 2, if i % 2 == 0 { TIF_GTC } else { TIF_IOC })
            }
        })
        .collect();
    let bids = (0..s.bids).map(|i| bid_leg(30 + i as u8, 25 + i as u8, 2, !(i + 1 == s.bids && s.last_bid_rests))).collect();
    (asks, bids)
}

fn state_for(actor: &Actor, asks: &[IntentAsk], bids: &[IntentBid]) -> (IntentState, u64, i64, u64) {
    let bought: i64 = asks.iter().map(|a| a.amount).sum();
    let quote: i64 = asks.iter().map(|a| quote_of(a.amount, a.order.state.price, a.order.state.scale, Round::Up).unwrap()).sum();
    let sold: i64 = bids.iter().map(|b| b.amount).sum();
    let released: i64 = bids.iter().map(|b| b.order.state.spend(b.amount, 0, 1_000).unwrap()).sum();
    let (payer, merchant) = (pk(PAYER), pk(MERCHANT));
    match actor.shape.kind {
        IntentKind::KasToToken => {
            let max_extra = (MCARRIER + 4 * FILLER + KAS / 2) as i64;
            (
                IntentState::KasToToken {
                    payer,
                    merchant,
                    token: TOK_B,
                    program: P8,
                    amount: bought,
                    max_pay: quote,
                    max_extra,
                    deadline: DEADLINE,
                },
                (quote + max_extra) as u64 + 5 * KAS,
                0,
                0,
            )
        }
        IntentKind::TokenToKas => {
            let merchant_kas = (released - KAS as i64 / 2) as u64;
            (
                IntentState::TokenToKas {
                    payer,
                    merchant,
                    token: TOK_A,
                    program: P8,
                    merchant_kas: merchant_kas as i64,
                    max_sell: sold,
                    deadline: DEADLINE,
                },
                KAS,
                sold,
                merchant_kas,
            )
        }
        IntentKind::TokenSwap => (
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a: TOK_A,
                program_a: P8,
                token_b: TOK_B,
                program_b: P8,
                max_sell_a: sold,
                amount_b: bought,
                deadline: DEADLINE,
            },
            2 * KAS,
            sold,
            0,
        ),
    }
}

fn intent_utxo(value: u64) -> Utxo {
    Utxo { transaction_id: [0x5d; 32], index: 0, amount: value, block_daa_score: 2_000, covenant_id: Some(INTENT_ID) }
}

fn lock_utxo(kind: IntentKind, lock: i64) -> Option<TokenUtxo> {
    kind.locks_tokens().then(|| tok_of(0x5e, lock, INTENT_ID, SCHEME_COVID, 2_000, TOK_A))
}

/// Every actor executed, and every kind expired: (name, built transaction).
pub fn intent_seeds() -> Vec<(String, BuiltTx)> {
    let mut v = vec![];
    for actor in ACTORS.iter().filter(|a| a.shape.a_family == kob_protocol::Family::Kcc20) {
        let (asks, bids) = legs_for(actor);
        let (state, value, lock, merchant_kas) = state_for(actor, &asks, &bids);
        let r = ExecuteIntent {
            actor: actor.name.into(),
            state: state.clone(),
            intent: intent_utxo(value),
            lock: lock_utxo(actor.shape.kind, lock),
            asks,
            bids,
            merchant_carrier: MCARRIER,
            merchant_kas,
            payer_token_carrier: None,
            filler: FILLER,
            keeper: pk(KEEPER_K),
            lock_time: NOW,
            records: vec![],
            fee: fee(),
        };
        if let Ok((b, _)) = build_execute_intent(&r, &intent_budgets) {
            v.push((format!("intent.execute.{}", actor.name), b));
        }
        if matches!(actor.name, "KasToToken_buy" | "TokenToKas_sell" | "TokenSwap_swap") {
            let e = ExpireIntent {
                actor: actor.name.into(),
                state,
                intent: intent_utxo(value),
                lock: lock_utxo(actor.shape.kind, lock),
                fee: fee(),
            };
            if let Ok(b) = build_expire_intent(&e, &intent_budgets) {
                v.push((format!("intent.expire.{}", actor.name), b));
            }
        }
    }
    v
}
