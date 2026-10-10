//! KOB router payment intents through the production builders (`build::{build_create_intent,
//! execute_intent, build_cancel_intent, build_expire_intent}`), validated by the rusty-kaspa v2.1.0 script engine with
//! enforced compute budgets, the storage-mass commitment and the fee floor.
//!
//! The router reads every token under the program the intent's state names (open ICC handles), so every test runs on
//! several token programs ([`RUNS`]): the 8/8 program KOB issues, the 3/3 reference program, a swap across the two,
//! the published public-mint build of the reference (3/3; token A and token B), and the KRON programs (2,433 B and 2,732 B) for a KRON token A (`TokenToKasKron_*`, `TokenSwapKron_*`, sold into
//! `KobBidKron`s; token B stays KCC-20).
//!
//! Positive: every one of the 30 router actors (fill shapes) is created, executed and validated on every program pair
//! that can run it (the 3/3 program has no room for a three-bid sell: the builders refuse it); each intent kind is
//! cancelled by its payer (token intents take their locked tokens back in the same transaction), and expired by anyone
//! from its deadline on. Negative: an execution whose merchant output or payer change is redirected after building, a
//! cancel signed by another key, a cancel that leaves the lock behind (one that spends other tokens of the same token
//! in its place: kob-tests' router harness, with its ablation), token B of another extension commitment than the intent names (the B pin), and a shape the orders do not fit are rejected (the router at the intent input, the builder for the last);
//! a KRON intent that locks no more than it may sell is refused at creation (its change output would be empty). The
//! covenant semantics of the router itself are covered exhaustively by `kob-tests/tests/argent_router_tests.rs`.

mod common;

use kaspa_consensus_core::tx::{Transaction, UtxoEntry};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::*;
use kob_protocol::router::{Actor, IntentKind, IntentState, ACTORS, EXPIRE_MAX_FEE};
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::*;
use kob_protocol::tx::*;
use kob_protocol::verify::{execute, validate};
use kob_protocol::Family;

use common::*;

const P8: TemplateId = TemplateId::Kcc20Ref8x8;
const P3: TemplateId = TemplateId::Kcc20Ref;
const K1: TemplateId = TemplateId::KronToken2433;
const K2: TemplateId = TemplateId::KronToken2732;
/// The published public-mint build of the reference (3/3, the app's context field in the template prefix).
const PM: TemplateId = TemplateId::Kcc20PublicMint;
const TOK_A: [u8; 32] = TOKEN_COV;
const TOK_B: [u8; 32] = TOKEN_B;
const PAYER: u8 = 11;
const KEEPER_K: u8 = 12;
const MCARRIER: u64 = KAS; // merchant token carrier
const FILLER: u64 = KAS / 5;
/// Every intent's deadline (unix ms).
const DEADLINE: i64 = 1_800_000_000_000;

/// The token programs of one run: token A (locked and sold) and token B (bought for the merchant, always KCC-20).
#[derive(Clone, Copy, Debug)]
struct Progs {
    a: TemplateId,
    b: TemplateId,
}
/// The program pairs every actor runs on (those of its token A family).
const RUNS: [Progs; 7] = [
    Progs { a: P8, b: P8 },
    Progs { a: P3, b: P3 },
    Progs { a: P3, b: P8 },
    Progs { a: K1, b: P8 },
    Progs { a: K2, b: P3 },
    Progs { a: PM, b: PM },
    Progs { a: K1, b: PM },
];

/// The runs of `actor`: a KasToToken intent has no token A (one run per token B program), a token intent runs on
/// every pair of its family. Pairs whose programs cannot run the shape are returned too (`fits` false).
fn runs_for(actor: &Actor) -> Vec<(Progs, bool)> {
    let mut out: Vec<(Progs, bool)> = vec![];
    for p in RUNS {
        let a = actor.shape.kind.locks_tokens().then_some(p.a);
        let b = actor.shape.kind.merchant_gets_token().then_some(p.b);
        if a.is_some_and(|a| a.family() != actor.shape.a_family) {
            continue;
        }
        if out.iter().any(|(q, _)| {
            a == actor.shape.kind.locks_tokens().then_some(q.a) && b == actor.shape.kind.merchant_gets_token().then_some(q.b)
        }) {
            continue;
        }
        out.push((p, actor.shape.fits(a, b)));
    }
    out
}

fn ask_b(maker: u8, n: i64, tif: i64, pb: TemplateId) -> AskState {
    AskState { token_cov_id: TOK_B, tif, ..ask_n(maker, P250, pb, n) }
}
fn bid_a(maker: u8, tif: i64, pa: TemplateId) -> BidState {
    BidState { tif, ..bid(maker, P245, pa) }
}
/// An ask of `n` whole tokens of B of which the execution takes `take` whole tokens.
fn ask_leg(tag: u8, maker: u8, n: i64, take: i64, tif: i64, pb: TemplateId) -> IntentAsk {
    let c = cov(0x50 + tag);
    IntentAsk {
        order: order(tag, CARRIER, c, 1_000, ask_b(maker, n, tif, pb)),
        custody: tok_of(tag + 100, n * WHOLE, c, SCHEME_COVID, 1_000, TOK_B),
        amount: take * WHOLE,
    }
}
/// A bid that buys `n` whole tokens and then rests (`ends = false`, funded for more) or ends (funded for `n` whole
/// tokens and half a minimum fill: it cannot afford another one).
fn bid_leg(tag: u8, maker: u8, n: i64, ends: bool, pa: TemplateId) -> IntentBid {
    let s = bid_a(maker, 0, pa);
    let value =
        if ends { s.escrow(n * WHOLE, 1).unwrap() + s.used(WHOLE).unwrap() / 2 } else { s.escrow((n + 5) * WHOLE, 1).unwrap() };
    IntentBid { order: order(tag, value as u64, cov(0x60 + tag), 1_000, s), amount: n * WHOLE }
}
/// A key-held token state of the payer's token A (KCC-20 owner scheme 0, KRON address presence).
fn user_a(pa: TemplateId, amount: i64, owner: [u8; 32]) -> TokenState {
    TokenState::user(pa.family(), amount, owner, ext_for(pa))
}
fn spk_on(p: TemplateId, st: &TokenState) -> kaspa_consensus_core::tx::ScriptPublicKey {
    st.spk_with(kob_protocol::artifacts::token_template(p))
}

/// Signs and validates a payer-built transaction (creation, cancel) and returns it with its entries.
fn signed(built: &BuiltTx) -> (Transaction, Vec<UtxoEntry>) {
    let sigs = sign_locally(built, &keys()).expect("sign");
    let s = finalize(built, &sigs, FinalizeOptions { tighten_budgets: true }).expect("finalize");
    let (tx, entries) = s.tx.to_tx().unwrap();
    validate(&tx, &entries).unwrap_or_else(|e| panic!("payer tx invalid: {e}"));
    (tx, entries)
}

struct Created {
    intent: Utxo,
    lock: Option<TokenUtxo>,
    state: IntentState,
}

fn create_req(actor: &Actor, state: &IntentState, p: Progs, value: u64, lock: i64) -> CreateIntent {
    let tokens = if actor.shape.kind.locks_tokens() { vec![tok_on(p.a, 90, lock + 3 * WHOLE, pk(PAYER), 900)] } else { vec![] };
    CreateIntent {
        actor: actor.name.into(),
        state: state.clone(),
        value,
        tokens,
        lock_amount: lock,
        lock_carrier: KAS,
        token_change_carrier: KAS,
        funding: vec![key_utxo(91, PAYER, 200 * KAS)],
        change: None,
        records: vec![kob_protocol::payload::Record::X402 { reference: vec![0xab; 32] }],
        fee: fee(),
    }
}

/// Creates an intent of `actor` with `state` (value `value`; token intents lock `lock` units).
fn create(actor: &Actor, state: IntentState, p: Progs, value: u64, lock: i64) -> Created {
    let r = create_req(actor, &state, p, value, lock);
    let built = build_create_intent(&r, &intent_budgets).unwrap_or_else(|e| panic!("{} {p:?}: create: {e}", actor.name));
    let id = built.covenants[0].covenant_id;
    let (tx, _) = signed(&built);
    let txid = tx.id().as_bytes();
    assert_eq!(tx.outputs[INTENT_OUTPUT as usize].script_public_key, state.spk(actor).unwrap(), "the intent output is the actor");
    assert_eq!(tx.outputs[0].covenant.unwrap().covenant_id.as_bytes(), id, "genesis");
    let intent = Utxo { transaction_id: txid, index: INTENT_OUTPUT, amount: value, block_daa_score: 2_000, covenant_id: Some(id) };
    let lock = actor.shape.kind.locks_tokens().then(|| {
        let o = &tx.outputs[LOCK_OUTPUT as usize];
        let st = TokenState::custody(p.a.family(), lock, id, ext_for(p.a));
        assert_eq!(o.script_public_key, spk_on(p.a, &st), "the locked tokens are owned by the intent");
        TokenUtxo {
            utxo: Utxo { transaction_id: txid, index: LOCK_OUTPUT, amount: o.value, block_daa_score: 2_000, covenant_id: Some(TOK_A) },
            state: st,
        }
    });
    Created { intent, lock, state }
}

fn exec_req(c: &Created, actor: &Actor, asks: Vec<IntentAsk>, bids: Vec<IntentBid>, merchant_kas: u64) -> ExecuteIntent {
    ExecuteIntent {
        actor: actor.name.into(),
        state: c.state.clone(),
        intent: c.intent.clone(),
        lock: c.lock.clone(),
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
    }
}

/// Orders that fit `actor`'s shape: asks that end sold out (everything they hold) except a
/// resting last one, bids that end (exhausted) except a resting last one.
fn legs_for(actor: &Actor, p: Progs) -> (Vec<IntentAsk>, Vec<IntentBid>) {
    let s = actor.shape;
    let asks = (0..s.asks)
        .map(|i| {
            let last = i + 1 == s.asks;
            if last && s.last_ask_rests {
                ask_leg(10 + i as u8, 20 + i as u8, 10, 2, TIF_GTC, p.b)
            } else {
                ask_leg(10 + i as u8, 20 + i as u8, 2, 2, if i % 2 == 0 { TIF_GTC } else { TIF_IOC }, p.b)
            }
        })
        .collect();
    let bids = (0..s.bids)
        .map(|i| {
            let last = i + 1 == s.bids;
            bid_leg(30 + i as u8, 25 + i as u8, 2, !(last && s.last_bid_rests), p.a)
        })
        .collect();
    (asks, bids)
}

/// The intent's terms for the legs, its value, the units it locks (KRON: one more than it sells, its change holds a
/// unit) and the merchant's KAS (TokenToKas).
fn state_for(actor: &Actor, p: Progs, asks: &[IntentAsk], bids: &[IntentBid]) -> (IntentState, u64, i64, u64) {
    let bought: i64 = asks.iter().map(|a| a.amount).sum();
    let quote: i64 = asks.iter().map(|a| quote_of(a.amount, a.order.state.price, a.order.state.scale, Round::Up).unwrap()).sum();
    let sold: i64 = bids.iter().map(|b| b.amount).sum();
    let released: i64 = bids.iter().map(|b| b.order.state.spend(b.amount, 0, 1_000).unwrap()).sum();
    let lock = sold + i64::from(p.a.family() == Family::Kron);
    let (payer, merchant) = (pk(PAYER), pk(MERCHANT));
    match actor.shape.kind {
        IntentKind::KasToToken => {
            let max_extra = (MCARRIER + 4 * FILLER + KAS / 2) as i64;
            (
                IntentState::KasToToken {
                    payer,
                    merchant,
                    token: TOK_B,
                    program: p.b,
                    amount: bought,
                    max_pay: quote,
                    max_extra,
                    b_extension: EXT,
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
                    program: p.a,
                    merchant_kas: merchant_kas as i64,
                    max_sell: sold,
                    lock_amount: lock,
                    lock_extension: ext_for(p.a),
                    deadline: DEADLINE,
                },
                KAS,
                lock,
                merchant_kas,
            )
        }
        IntentKind::TokenSwap => (
            IntentState::TokenSwap {
                payer,
                merchant,
                token_a: TOK_A,
                program_a: p.a,
                token_b: TOK_B,
                program_b: p.b,
                max_sell_a: sold,
                amount_b: bought,
                lock_amount: lock,
                lock_extension: ext_for(p.a),
                b_extension: EXT,
                deadline: DEADLINE,
            },
            2 * KAS,
            lock,
            0,
        ),
    }
}

fn validate_exec(r: &ExecuteIntent) -> Result<(Transaction, Vec<UtxoEntry>, ExecutionFacts), String> {
    let (s, facts) = execute_intent(r).map_err(|e| e.to_string())?;
    let (tx, entries) = s.tx.to_tx().map_err(|e| e.to_string())?;
    validate(&tx, &entries).map_err(|e| e.to_string())?;
    Ok((tx, entries, facts))
}

/// Every actor on every program pair of its family that can run it; the 3/3 program refuses the three-bid sells
/// (the builder refuses the intent before anything is locked).
#[test]
fn every_router_shape_is_created_executed_and_validated() {
    let mut ran = 0;
    for actor in &ACTORS {
        for (p, fits) in runs_for(actor) {
            let (asks, bids) = legs_for(actor, p);
            let (state, value, lock, merchant_kas) = state_for(actor, p, &asks, &bids);
            if !fits {
                assert!(matches!(p.a, P3 | PM) && actor.shape.bids == 3, "{} {p:?}: only a 3/3 program refuses a shape", actor.name);
                let e = build_create_intent(&create_req(actor, &state, p, value, lock), &intent_budgets).unwrap_err();
                assert!(e.to_string().contains("no room"), "{} {p:?}: {e}", actor.name);
                continue;
            }
            let c = create(actor, state, p, value, lock);
            let r = exec_req(&c, actor, asks, bids, merchant_kas);
            let (tx, _, facts) = validate_exec(&r).unwrap_or_else(|e| panic!("{} {p:?}: {e}", actor.name));
            let me = tx.inputs.len() - 1;
            assert_eq!(tx.inputs[me].previous_outpoint.index, INTENT_OUTPUT, "{}: the intent is the last input", actor.name);
            let mo = &tx.outputs[facts.merchant_output as usize];
            match actor.shape.kind {
                IntentKind::TokenToKas => {
                    assert_eq!(facts.merchant_output as usize, me);
                    assert_eq!((mo.script_public_key.clone(), mo.value), (p2pk_spk(&pk(MERCHANT)), merchant_kas));
                }
                IntentKind::KasToToken => {
                    assert_eq!(facts.merchant_output as usize, me + 1);
                    assert_eq!(tx.outputs[me].script_public_key, p2pk_spk(&pk(PAYER)), "payer change at j");
                    assert_eq!(mo.value, MCARRIER);
                }
                IntentKind::TokenSwap => {
                    assert_eq!(facts.merchant_output as usize, me);
                    assert_eq!(mo.value, MCARRIER);
                }
            }
            if let Some(l) = &c.lock {
                // the payer's change of token A at j + 1, on A's program and of A's owner type
                let change = user_a(p.a, l.state.amount() - facts.sold, pk(PAYER));
                assert_eq!(tx.outputs[me + 1].script_public_key, spk_on(p.a, &change), "{}: payer change at j + 1", actor.name);
            }
            println!(
                "{:<26} {:>15} -> {:<14} ok: {} in / {} out, {} B, intent value {} sompi",
                actor.name,
                if actor.shape.kind.locks_tokens() { p.a.name() } else { "KAS" },
                if actor.shape.kind.merchant_gets_token() { p.b.name() } else { "KAS" },
                tx.inputs.len(),
                tx.outputs.len(),
                kaspa_consensus_core::mass::transaction_estimated_serialized_size(&tx),
                r.intent.amount
            );
            ran += 1;
        }
    }
    assert!(ran >= 30 + 12 + 6, "every actor ran on its programs ({ran} runs)");
}

/// Redirected merchant outputs, a shrunk payer change, the payer's token change to a thief, and an input after the
/// intent: rejected by the router at the intent input, on every program family.
#[test]
fn redirected_outputs_are_rejected_by_the_router() {
    for (name, p) in [
        ("KasToToken_buy", RUNS[0]),
        ("KasToToken_buy2", RUNS[1]),
        ("TokenToKas_sell", RUNS[0]),
        ("TokenToKas_sell2_out", RUNS[1]),
        ("TokenSwap_swap", RUNS[2]),
        ("TokenToKasKron_sell", RUNS[3]),
        ("TokenToKasKron_sell3", RUNS[4]),
        ("TokenSwapKron_swap", RUNS[3]),
        ("TokenSwapKron_swap2_out", RUNS[4]),
    ] {
        let actor = Actor::by_name(name).unwrap();
        let (asks, bids) = legs_for(actor, p);
        let (state, value, lock, merchant_kas) = state_for(actor, p, &asks, &bids);
        let c = create(actor, state, p, value, lock);
        let r = exec_req(&c, actor, asks, bids, merchant_kas);
        let (tx, entries, facts) = validate_exec(&r).unwrap();
        let me = tx.inputs.len() - 1;
        // the merchant output to a thief: same value, another key (KAS) / owner (token)
        let mut bad = tx.clone();
        let mo = facts.merchant_output as usize;
        bad.outputs[mo].script_public_key = match actor.shape.kind {
            IntentKind::TokenToKas => p2pk_spk(&pk(30)),
            _ => spk_on(p.b, &TokenState::user(Family::Kcc20, facts.bought, pk(30), EXT)),
        };
        let res = execute(&bad, &entries, true).unwrap();
        assert!(res[me].is_err(), "{name}: a redirected merchant output must fail at the intent input");
        // KasToToken: the payer's change shrunk (the keeper takes it)
        if actor.shape.kind == IntentKind::KasToToken {
            let mut bad = tx.clone();
            let cut = bad.outputs[me].value / 2;
            bad.outputs[me].value -= cut;
            let res = execute(&bad, &entries, true).unwrap();
            assert!(res[me].is_err(), "{name}: the payer's change below the bound must fail at the intent input");
        }
        // positional rule: the intent is the last input (an input after it could claim output j + 1)
        let mut bad = tx.clone();
        let mut extra = tx.inputs[0].clone();
        extra.previous_outpoint.index = 99;
        bad.inputs.push(extra);
        let mut bad_entries = entries.clone();
        bad_entries.push(entries[0].clone());
        let res = execute(&bad, &bad_entries, true).unwrap();
        assert!(res[me].is_err(), "{name}: an input after the intent must fail at the intent input");
        // token intents: the payer's token change to a thief (same amount and owner type)
        if let Some(l) = &c.lock {
            let mut bad = tx.clone();
            bad.outputs[me + 1].script_public_key = spk_on(p.a, &user_a(p.a, l.state.amount() - facts.sold, pk(30)));
            let res = execute(&bad, &entries, true).unwrap();
            assert!(res[me].is_err(), "{name}: the payer's token change to a thief must fail at the intent input");
        }
    }
}

#[test]
fn a_shape_the_orders_do_not_fit_is_refused_by_the_builder() {
    // a resting ask under buy_out, a sold-out ask under buy
    let p = RUNS[0];
    let buy = Actor::by_name("KasToToken_buy").unwrap();
    let out = Actor::by_name("KasToToken_buy_out").unwrap();
    let (asks, _) = legs_for(buy, p);
    let (state, value, ..) = state_for(buy, p, &asks, &[]);
    let c = create(out, state.clone(), p, value, 0);
    let e = build_execute_intent(&exec_req(&c, out, asks, vec![], 0), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("sold out"), "{e}");
    let (asks, _) = legs_for(out, p);
    let c = create(buy, state, p, value, 0);
    let e = build_execute_intent(&exec_req(&c, buy, asks, vec![], 0), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("rest"), "{e}");
}

/// The programs an intent names are the programs it trades: a KRON intent on a KCC-20 actor (and the reverse), an
/// order of another program than the intent's, and KaspaCom's program (pending review) are refused by the builders.
#[test]
fn the_intent_programs_bind_actor_and_orders() {
    let kron = Actor::by_name("TokenToKasKron_sell").unwrap();
    let kcc = Actor::by_name("TokenToKas_sell").unwrap();
    let (asks, bids) = legs_for(kron, RUNS[3]);
    let (st_kron, value, lock, _) = state_for(kron, RUNS[3], &asks, &bids);
    let e = build_create_intent(&create_req(kcc, &st_kron, RUNS[3], value, lock), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("does not trade"), "{e}");
    let (asks, bids) = legs_for(kcc, RUNS[0]);
    let (st_kcc, value, lock, mk) = state_for(kcc, RUNS[0], &asks, &bids);
    let e = build_create_intent(&create_req(kron, &st_kcc, RUNS[0], value, lock), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("does not trade"), "{e}");
    // a bid of the 3/3 program into an intent that sells tokens of the 8/8 one
    let c = create(kcc, st_kcc.clone(), RUNS[0], value, lock);
    let (_, bids3) = legs_for(kcc, RUNS[1]);
    let e = build_execute_intent(&exec_req(&c, kcc, vec![], bids3, mk), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("program"), "{e}");
    // KaspaCom's program, pending review
    let mut kc = st_kcc;
    if let IntentState::TokenToKas { program, .. } = &mut kc {
        *program = TemplateId::Kcc20KaspaCom025;
    }
    assert!(kc.check().unwrap_err().to_string().contains("pending review"));
}

/// A KRON output holds at least one unit: a KRON intent that locks no more than it may sell is refused at creation
/// (its payer change after a sale of max_sell would be empty), and an execution that sells the whole lock is refused by
/// the builder (the token program would reject it).
#[test]
fn a_kron_intent_locks_more_than_it_may_sell() {
    let actor = Actor::by_name("TokenToKasKron_sell2").unwrap();
    let p = RUNS[3];
    let (asks, bids) = legs_for(actor, p);
    let (state, value, lock, merchant_kas) = state_for(actor, p, &asks, &bids);
    let e = build_create_intent(&create_req(actor, &state, p, value, lock - 1), &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("locks more than it may sell"), "{e}");
    // a lock of exactly the units sold (a third-party creation): its state pins lock_amount = max_sell, which the
    // builder refuses (an execution would empty the lock)
    let c = create(actor, state, p, value, lock);
    let mut r = exec_req(&c, actor, asks, bids, merchant_kas);
    let l = r.lock.as_mut().unwrap();
    l.state = l.state.with_amount(lock - 1);
    if let IntentState::TokenToKas { lock_amount, .. } = &mut r.state {
        *lock_amount = lock - 1;
    }
    let e = build_execute_intent(&r, &intent_budgets).unwrap_err();
    assert!(e.to_string().contains("locks more than it may sell"), "{e}");
}

#[test]
fn the_payer_cancels_every_intent_kind() {
    for (name, p) in [
        ("KasToToken_buy2", RUNS[0]),
        ("KasToToken_buy", RUNS[1]),
        ("TokenToKas_sell", RUNS[0]),
        ("TokenToKas_sell2", RUNS[1]),
        ("TokenSwap_swap2_out", RUNS[2]),
        ("TokenToKasKron_sell", RUNS[3]),
        ("TokenToKasKron_sell3_out", RUNS[4]),
        ("TokenSwapKron_swap", RUNS[4]),
    ] {
        let actor = Actor::by_name(name).unwrap();
        let (asks, bids) = legs_for(actor, p);
        let (state, value, lock, _) = state_for(actor, p, &asks, &bids);
        let c = create(actor, state.clone(), p, value, lock);
        let r = CancelIntent {
            actor: name.into(),
            state: state.clone(),
            intent: c.intent.clone(),
            lock: c.lock.clone(),
            to: None,
            fee: fee(),
        };
        let built = build_cancel_intent(&r, &intent_budgets).unwrap();
        let (tx, entries) = signed(&built);
        let units = kob_protocol::verify::measure_units(&tx, &entries).unwrap();
        let need = kob_protocol::budget::budget_for_units(units[0]);
        assert!(need <= ROUTER_CANCEL_BUDGET, "{name}: cancel needs budget {need}, ROUTER_CANCEL_BUDGET is {ROUTER_CANCEL_BUDGET}");
        if let Some(l) = &c.lock {
            let back = tx.outputs.iter().find(|o| o.covenant.is_some_and(|b| b.covenant_id.as_bytes() == TOK_A)).unwrap();
            assert_eq!(
                back.script_public_key,
                spk_on(p.a, &user_a(p.a, l.state.amount(), pk(PAYER))),
                "{name}: tokens back to the payer"
            );
        }
        // signed by another key: the router rejects at the intent input
        let digest = sighash(&tx, &entries, 0);
        let thief_sig = sign_digest(&sk(30), &digest).unwrap();
        let mut bad = tx.clone();
        bad.inputs[0].signature_script = built.plans[0].sigscript(Some(&thief_sig)).unwrap();
        let res = execute(&bad, &entries, true).unwrap();
        assert!(res[0].is_err(), "{name}: a cancel by another key must fail");
        if c.lock.is_some() {
            // C5 X-1: the router itself refuses a token intent's cancel that leaves the lock behind (the cancel ends the
            // covenant: tokens owned by the intent id could never move again), even signed by the payer
            let tok_out = tx.outputs.iter().position(|o| o.covenant.is_some_and(|b| b.covenant_id.as_bytes() == TOK_A)).unwrap();
            let mut orphan = tx.clone();
            orphan.inputs.remove(1);
            orphan.outputs.remove(tok_out);
            let mut orphan_entries = entries.clone();
            orphan_entries.remove(1);
            let sig = sign_digest(&sk(PAYER), &sighash(&orphan, &orphan_entries, 0)).unwrap();
            orphan.inputs[0].signature_script = built.plans[0].sigscript(Some(&sig)).unwrap();
            let res = execute(&orphan, &orphan_entries, true).unwrap();
            assert!(res[0].is_err(), "{name}: a cancel without the lock must fail at the router");
        }
    }
}

/// `ROUTER_CANCEL_BUDGET` covers the cancel of every actor and is at most one unit above the largest need.
#[test]
fn the_cancel_budget_covers_every_actor() {
    let mut most = 0;
    for actor in &ACTORS {
        for (p, fits) in runs_for(actor) {
            if !fits {
                continue;
            }
            let (asks, bids) = legs_for(actor, p);
            let (state, value, lock, _) = state_for(actor, p, &asks, &bids);
            let c = create(actor, state.clone(), p, value, lock);
            let r =
                CancelIntent { actor: actor.name.into(), state, intent: c.intent.clone(), lock: c.lock.clone(), to: None, fee: fee() };
            let (tx, entries) = signed(&build_cancel_intent(&r, &intent_budgets).unwrap());
            let need = kob_protocol::budget::budget_for_units(kob_protocol::verify::measure_units(&tx, &entries).unwrap()[0]);
            println!("{:<26} {:?} cancel budget {need}", actor.name, p);
            most = most.max(need);
        }
    }
    assert!(
        most <= ROUTER_CANCEL_BUDGET && ROUTER_CANCEL_BUDGET <= most + 1,
        "largest cancel need {most}, ROUTER_CANCEL_BUDGET {ROUTER_CANCEL_BUDGET}"
    );
}

/// Anyone expires every actor's intent from its deadline on, without a signature: the lock time is the deadline, the KAS
/// goes back to the payer (less at most `EXPIRE_MAX_FEE`) and the locked tokens, whole, to the payer's key. Before the
/// deadline, or with the KAS or the tokens redirected, the router rejects at the intent input. `ROUTER_EXPIRE_BUDGET`
/// covers every actor and is at most one unit above the largest need.
#[test]
fn every_intent_is_expired_from_its_deadline() {
    let mut most = 0;
    for actor in &ACTORS {
        for (p, fits) in runs_for(actor) {
            if !fits {
                continue;
            }
            let (asks, bids) = legs_for(actor, p);
            let (state, value, lock, _) = state_for(actor, p, &asks, &bids);
            let c = create(actor, state.clone(), p, value, lock);
            let r = ExpireIntent { actor: actor.name.into(), state, intent: c.intent.clone(), lock: c.lock.clone(), fee: fee() };
            let built = build_expire_intent(&r, &intent_budgets).unwrap_or_else(|e| panic!("{}: {e}", actor.name));
            assert!(built.sign.is_empty(), "{}: an expiry needs no signature", actor.name);
            let s = finalize(&built, &[], FinalizeOptions { tighten_budgets: true }).unwrap();
            let (tx, entries) = s.tx.to_tx().unwrap();
            validate(&tx, &entries).unwrap_or_else(|e| panic!("{} {p:?}: expiry invalid: {e}", actor.name));
            assert_eq!(tx.lock_time, DEADLINE as u64, "{}: the lock time is the deadline", actor.name);
            assert_eq!(tx.outputs[0].script_public_key, p2pk_spk(&pk(PAYER)), "{}: the KAS back to the payer", actor.name);
            assert!(tx.outputs[0].value + EXPIRE_MAX_FEE >= c.intent.amount, "{}: the fee is within EXPIRE_MAX_FEE", actor.name);
            if let Some(l) = &c.lock {
                assert_eq!(tx.outputs[1].script_public_key, spk_on(p.a, &user_a(p.a, l.state.amount(), pk(PAYER))), "{}", actor.name);
            }
            let need = kob_protocol::budget::budget_for_units(kob_protocol::verify::measure_units(&tx, &entries).unwrap()[0]);
            println!(
                "{:<26} {:?} expire budget {need}, {} B",
                actor.name,
                p,
                kaspa_consensus_core::mass::transaction_estimated_serialized_size(&tx)
            );
            most = most.max(need);
            // one millisecond before the deadline
            let mut early = tx.clone();
            early.lock_time = DEADLINE as u64 - 1;
            assert!(execute(&early, &entries, true).unwrap()[0].is_err(), "{}: an expiry before the deadline must fail", actor.name);
            // the KAS to a thief
            let mut bad = tx.clone();
            bad.outputs[0].script_public_key = p2pk_spk(&pk(30));
            assert!(execute(&bad, &entries, true).unwrap()[0].is_err(), "{}: a redirected expiry must fail", actor.name);
            // the tokens to a thief
            if let Some(l) = &c.lock {
                let mut bad = tx.clone();
                bad.outputs[1].script_public_key = spk_on(p.a, &user_a(p.a, l.state.amount(), pk(30)));
                assert!(execute(&bad, &entries, true).unwrap()[0].is_err(), "{}: redirected tokens must fail", actor.name);
            }
        }
    }
    assert!(
        most <= ROUTER_EXPIRE_BUDGET && ROUTER_EXPIRE_BUDGET <= most + 1,
        "largest expire need {most}, ROUTER_EXPIRE_BUDGET {ROUTER_EXPIRE_BUDGET}"
    );
}

/// C5 X-7: an intent's cancel and expiry take their fee from the intent's own KAS (no funding input), so an intent below
/// `MIN_INTENT_VALUE` could be neither cancelled nor expired: the builder refuses to create one.
#[test]
fn an_intent_too_small_to_exit_is_not_created() {
    let actor = Actor::by_name("TokenToKas_sell").unwrap();
    let p = RUNS[0];
    let (asks, bids) = legs_for(actor, p);
    let (state, _, lock, _) = state_for(actor, p, &asks, &bids);
    let r =
        CreateIntent { value: kob_protocol::router::MIN_INTENT_VALUE - 1, records: vec![], ..create_req(actor, &state, p, 0, lock) };
    let e = build_create_intent(&r, &intent_budgets).unwrap_err().to_string();
    assert!(e.contains("MIN_INTENT_VALUE"), "{e}");
    let ok = CreateIntent { value: kob_protocol::router::MIN_INTENT_VALUE, ..r };
    build_create_intent(&ok, &intent_budgets).expect("an intent of exactly MIN_INTENT_VALUE is created");
}

/// The lock pin: the state names the lock's exact units and extension commitment. The builders refuse a creation whose
/// state pins another lock than the one it creates, and an expiry, cancel or execution that spends a token UTXO owned by
/// the intent other than its lock (a stand-in anyone can send to the intent's id; the router refuses it too:
/// `argent_router_tests.rs`, `router_lock_pin`).
#[test]
fn the_lock_pin_is_the_created_lock() {
    for (name, p) in [("TokenToKas_sell", RUNS[0]), ("TokenSwap_swap", RUNS[2]), ("TokenToKasKron_sell", RUNS[3])] {
        let actor = Actor::by_name(name).unwrap();
        let (asks, bids) = legs_for(actor, p);
        let (state, value, lock, _) = state_for(actor, p, &asks, &bids);
        let e = build_create_intent(&create_req(actor, &state, p, value, lock + 1), &intent_budgets).unwrap_err();
        assert!(e.to_string().contains("lock pin"), "{name}: {e}");
        let c = create(actor, state.clone(), p, value, lock);
        let l = c.lock.clone().unwrap();
        let mut dust = l.clone();
        dust.state = dust.state.with_amount(1);
        let r = ExpireIntent { actor: name.into(), state: state.clone(), intent: c.intent.clone(), lock: Some(dust), fee: fee() };
        let e = build_expire_intent(&r, &intent_budgets).unwrap_err();
        assert!(e.to_string().contains("not the intent's lock"), "{name}: {e}");
        let r = ExpireIntent { actor: name.into(), state, intent: c.intent.clone(), lock: Some(l), fee: fee() };
        build_expire_intent(&r, &intent_budgets).unwrap_or_else(|e| panic!("{name}: the lock itself expires: {e}"));
    }
}

// ---------------------------------------------------------------------------------------------- the B pin

/// Another extension commitment than the fixture token's (`EXT`): units of token B of another class (the same covenant
/// id; only the token's issuer can create them, in its genesis).
const OTHER_EXT: [u8; 32] = [0xdd; 32];

/// `state` naming `ext` as the extension commitment of token B.
fn with_b_extension(state: &IntentState, ext: [u8; 32]) -> IntentState {
    let mut s = state.clone();
    match &mut s {
        IntentState::KasToToken { b_extension, .. } | IntentState::TokenSwap { b_extension, .. } => *b_extension = ext,
        IntentState::TokenToKas { .. } => panic!("a TokenToKas intent buys no token B"),
    }
    s
}

/// The asks with custodies of extension commitment `ext`.
fn asks_of_class(asks: &[IntentAsk], ext: [u8; 32]) -> Vec<IntentAsk> {
    let mut v = asks.to_vec();
    for a in &mut v {
        // an ask pins the commitment of its custody (KobAsk extensionCommitment)
        a.order.state.extension_commitment = ext;
        match &mut a.custody.state {
            TokenState::Kcc20(k) => k.extension_commitment = ext,
            other => panic!("token B is KCC-20, not {other:?}"),
        }
    }
    v
}

/// Signs every input of a built transaction as built (no budget tightening) and returns the engine's view of it.
fn sealed(b: &BuiltTx) -> (Transaction, Vec<UtxoEntry>) {
    let keys = keys();
    let (mut tx, entries) = b.tx.to_tx().unwrap();
    for (i, plan) in b.plans.iter().enumerate() {
        let sig = plan.signer().map(|k| sign_digest(&keys[&k], &sighash(&tx, &entries, i)).unwrap());
        tx.inputs[i].signature_script = plan.sigscript(sig.as_deref()).unwrap();
    }
    (tx, entries)
}

/// The inputs of `tx` the script engine refuses.
fn failing(tx: &Transaction, entries: &[UtxoEntry]) -> std::collections::BTreeSet<usize> {
    execute(tx, entries, false).unwrap().iter().enumerate().filter(|(_, r)| r.is_err()).map(|(i, _)| i).collect()
}

/// The asks of `b` (inputs of the KobAsk template) with their state's `extensionCommitment` set to `ext`: the order input,
/// its plan and the continuation of a resting ask (`amountLeft - n`). The execution a filler could build from its own
/// KobAsk of units of B of another class (an ask pins the commitment of its custody, which can be any commitment of the
/// covenant id). Returns the ask inputs.
fn reclass_asks(b: &mut BuiltTx, ext: [u8; 32]) -> std::collections::BTreeSet<usize> {
    let mut asks = std::collections::BTreeSet::new();
    for i in 0..b.plans.len() {
        let SigPlan::Entry { template: TemplateId::KobAsk, state, args, .. } = &mut b.plans[i] else { continue };
        let old = AskState::decode(state).unwrap();
        let new = AskState { extension_commitment: ext, ..old.clone() };
        *state = new.encode();
        b.tx.inputs[i].utxo.script_public_key = spk_to_string(&AnyState::KobAsk(new.clone()).spk());
        let n = match args.first() {
            Some(Arg::Bytes(x)) => i64::from_le_bytes(x.as_slice().try_into().unwrap()),
            other => panic!("ask fill argument {other:?}"),
        };
        let rest = |a: &AskState| spk_to_string(&AnyState::KobAsk(AskState { amount_left: a.amount_left - n, ..a.clone() }).spk());
        let (old_rest, new_rest) = (rest(&old), rest(&new));
        for o in b.tx.outputs.iter_mut().filter(|o| o.script_public_key == old_rest) {
            o.script_public_key = new_rest.clone();
        }
        asks.insert(i);
    }
    asks
}

/// Gives every token-B input and output of `b` the extension commitment `ext`: the asks' escrows, an escrow rest and
/// the merchant's delivery, with the token program's next states rewritten so that its inputs stay valid (the asks keep
/// their state: [`reclass_asks`]).
fn reclass_b(b: &mut BuiltTx, prog_b: TemplateId, ext: [u8; 32]) {
    let tpl = kob_protocol::artifacts::token_template(prog_b);
    let mut next: Option<Vec<Kcc20State>> = None;
    for i in 0..b.plans.len() {
        if b.tx.inputs[i].utxo.covenant_id != Some(TOK_B) {
            continue;
        }
        match &mut b.plans[i] {
            SigPlan::TokenLeader { state, next_states, .. } => {
                state.extension_commitment = ext;
                for n in next_states.iter_mut() {
                    n.extension_commitment = ext;
                }
                next = Some(next_states.clone());
                b.tx.inputs[i].utxo.script_public_key = spk_to_string(&TokenState::Kcc20(state.clone()).spk_with(tpl));
            }
            SigPlan::TokenDelegator { state, .. } => {
                state.extension_commitment = ext;
                b.tx.inputs[i].utxo.script_public_key = spk_to_string(&TokenState::Kcc20(state.clone()).spk_with(tpl));
            }
            other => panic!("input {i} of token B is not a token input: {other:?}"),
        }
    }
    let next = next.expect("a token-B leader");
    let outs: Vec<usize> =
        (0..b.tx.outputs.len()).filter(|&o| b.tx.outputs[o].covenant.as_ref().is_some_and(|c| c.covenant_id == TOK_B)).collect();
    assert_eq!(outs.len(), next.len());
    for (o, st) in outs.into_iter().zip(next) {
        b.tx.outputs[o].script_public_key = spk_to_string(&TokenState::Kcc20(st).spk_with(tpl));
    }
}

/// The B pin (`router_head.ag`, "B PIN"): an intent that buys token B (KasToToken, TokenSwap, TokenSwapKron) takes it
/// only from ask escrows of the extension commitment its state names (`b_extension`, the offer's token), and the
/// merchant's delivery carries that commitment. Every such actor on every program pair it runs on:
///  - the execution of escrows of the named commitment validates on every input;
///  - the same execution with every token-B UTXO of another commitment of the same covenant id (escrows, escrow rest,
///    delivery) is refused at the intent input and at every ask (each pins its own custody's commitment); from asks that
///    name that commitment it is refused at the intent input, and only there (the asks and the token program accept it);
///  - the builder refuses escrows of another commitment;
///  - an intent that names that other commitment takes it (the pin follows the state, not a constant).
#[test]
fn an_intent_takes_token_b_only_of_its_extension_commitment() {
    let mut runs = 0;
    for actor in ACTORS.iter().filter(|a| a.shape.kind.merchant_gets_token()) {
        for (p, fits) in runs_for(actor) {
            if !fits {
                continue;
            }
            let (asks, bids) = legs_for(actor, p);
            let (state, value, lock, merchant_kas) = state_for(actor, p, &asks, &bids);
            assert_eq!(state.b_extension(), Some(EXT));
            let c = create(actor, state.clone(), p, value, lock);
            let r = exec_req(&c, actor, asks.clone(), bids.clone(), merchant_kas);
            let (built, facts) = build_execute_intent(&r, &intent_budgets).unwrap_or_else(|e| panic!("{} {p:?}: {e}", actor.name));
            let (tx, entries) = sealed(&built);
            assert!(failing(&tx, &entries).is_empty(), "{} {p:?}: escrows of the named commitment", actor.name);
            let me = tx.inputs.len() - 1;
            let delivered = TokenState::user(Family::Kcc20, facts.bought, pk(MERCHANT), EXT);
            assert_eq!(tx.outputs[facts.merchant_output as usize].script_public_key, spk_on(p.b, &delivered));

            let mut other = built.clone();
            reclass_b(&mut other, p.b, OTHER_EXT);
            let (tx2, entries2) = sealed(&other);
            let mut asks_in = reclass_asks(&mut built.clone(), EXT);
            assert!(!asks_in.is_empty());
            asks_in.insert(me);
            assert_eq!(
                failing(&tx2, &entries2),
                asks_in,
                "{} {p:?}: token B of another commitment is refused at the intent input and at every ask",
                actor.name
            );
            reclass_asks(&mut other, OTHER_EXT);
            let (tx2, entries2) = sealed(&other);
            assert_eq!(
                failing(&tx2, &entries2),
                std::collections::BTreeSet::from([me]),
                "{} {p:?}: token B of another commitment from asks that name it is refused at the intent input, and only there",
                actor.name
            );

            let e = build_execute_intent(
                &exec_req(&c, actor, asks_of_class(&asks, OTHER_EXT), bids.clone(), merchant_kas),
                &intent_budgets,
            )
            .unwrap_err();
            assert!(e.to_string().contains("b_extension"), "{} {p:?}: {e}", actor.name);

            let named = with_b_extension(&state, OTHER_EXT);
            let c2 = create(actor, named, p, value, lock);
            let r2 = exec_req(&c2, actor, asks_of_class(&asks, OTHER_EXT), bids, merchant_kas);
            let (built2, facts2) = build_execute_intent(&r2, &intent_budgets).unwrap_or_else(|e| panic!("{} {p:?}: {e}", actor.name));
            let (tx3, entries3) = sealed(&built2);
            assert!(failing(&tx3, &entries3).is_empty(), "{} {p:?}: an intent naming the other commitment takes it", actor.name);
            let delivered = TokenState::user(Family::Kcc20, facts2.bought, pk(MERCHANT), OTHER_EXT);
            assert_eq!(tx3.outputs[facts2.merchant_output as usize].script_public_key, spk_on(p.b, &delivered));
            runs += 1;
        }
    }
    assert!(runs >= 18, "every actor that buys token B ran ({runs} runs)");
}
