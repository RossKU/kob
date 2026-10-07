//! Builders for KOB router payment intents (`crate::router`): create (the payer, one signature),
//! execute (any keeper, no signature), cancel (the payer) and expire (anyone, from the deadline on,
//! no signature).
//!
//! Layouts follow the router's positional anti-aliasing rule (`docs/argent.md`, "Positional
//! anti-aliasing rule"): the intent is the LAST input `j` and owns outputs `j` and `j + 1`; every
//! order spent at input `i` pays, delivers or refunds at output `i`.
//!
//! ```text
//! KasToToken  (k asks, j = 2k)
//!   in   [0..k) asks  [k..2k) their custodies (leader first)  [2k] intent
//!   out  [0..k) maker payouts  [k] custody rest of the last ask (if it rests), fillers up to 2k
//!        [2k] payer KAS change  [2k+1] merchant token delivery  [2k+2] last ask continuation (if it rests)
//! TokenToKas  (k bids, j = k + 1)
//!   in   [0..k) bids  [k] locked token A (owned by the intent)  [k+1] intent
//!   out  [0..k) deliveries to the bid makers  [k] last bid continuation (if it rests) or a filler
//!        [k+1] merchant KAS  [k+2] payer token A change  then the keeper's KAS
//! TokenSwap   (kb bids, ka asks, a0 = kb + ka, j = a0 + 1 + ka)
//!   in   [0..kb) bids  [kb..a0) asks  [a0] locked token A  [a0+1..j) custodies of B (leader first)  [j] intent
//!   out  [0..kb) A deliveries  [kb..a0) ask payouts  [a0..j) the first ka + 1 of: custody rest of B,
//!        bid continuation, ask continuation (fillers when fewer)  [j] merchant B delivery
//!        [j+1] payer token A change  then the remaining continuations, then the keeper's KAS
//! ```
//!
//! Every token is of the program the intent's state names (`IntentState::locked_program`, `merchant_program`;
//! the router reads and pins it under that handle). A KRON token A (`TokenToKasKron_*`, `TokenSwapKron_*`) is
//! locked with id_type 2 and sold into `KobBidKron`s; its deliveries and the payer's change are id_type 3. KRON
//! has no token leader: every KRON input names its authorising input (the lock: the intent; key-held tokens of the
//! payer: a P2PK input of the same key, so a creation that spends them must be funded from that key).

use serde::{Deserialize, Serialize};

use super::{check_custody, check_token_input, nb, pos, token_witness, BudgetFn, Token};
use crate::artifacts::TemplateId;
use crate::error::{invalid, Error, Result};
use crate::family::Family;
use crate::payload::{self, Record};
use crate::router::{cancel_args, expire_args, fill_args, Actor, FillArgs, IntentKind, IntentState, EXPIRE_MAX_FEE, MIN_INTENT_VALUE};
use crate::script::p2pk_spk;
use crate::state::{quote_of, AskState, BidState, OrderState, Round, TokenState, TIF_FOK, TIF_GTC};
use crate::tx::{
    check_key, Arg, BuiltTx, Draft, FeeOptions, KeyUtxo, OrderUtxo, SigPlan, TokenUtxo, Utxo, Witness, SEQUENCE_NONFINAL,
};

/// Index of the intent output in a creation transaction.
pub const INTENT_OUTPUT: u32 = 0;
/// Index of the locked token output in a creation transaction (token intents).
pub const LOCK_OUTPUT: u32 = 1;

/// Budget role of a router input.
pub fn router_role(actor: &Actor, entry: &str) -> String {
    format!("router.{}.{entry}", actor.name)
}

fn actor_of(name: &str) -> Result<&'static Actor> {
    Actor::by_name(name).ok_or_else(|| Error::Invalid(format!("{name} is not a router actor")))
}

/// The token the payer locks (token A) with its program, as the intent's state names it.
fn locked_token(st: &IntentState) -> Option<Token> {
    st.locked_token().zip(st.locked_program())
}

/// The lock pin (router_head.ag, "LOCK PIN"): a token UTXO owned by the intent is its lock only with the state's exact
/// `lock_amount` and `lock_extension`; the router refuses any other (a stand-in sent to the intent's id).
fn check_lock_pin(st: &IntentState, l: &TokenUtxo) -> Result<()> {
    match st.lock_pin() {
        Some((amount, ext)) if l.state.amount() == amount && l.state.extension() == ext => Ok(()),
        Some((amount, _)) => invalid(format!(
            "the token UTXO is not the intent's lock: it holds {} units (lock_amount {amount}) or another extension commitment",
            l.state.amount()
        )),
        None => invalid("a KasToToken intent locks no tokens"),
    }
}

/// The token the merchant receives (token B) with its program, as the intent's state names it.
fn merchant_token(st: &IntentState) -> Option<Token> {
    st.merchant_token().zip(st.merchant_program())
}

fn int_args(v: Vec<silverscript_abi::ArtifactValue>) -> Result<Vec<Arg>> {
    v.into_iter()
        .map(|v| match v {
            silverscript_abi::ArtifactValue::Int(i) => Ok(Arg::Int(i)),
            other => Err(Error::Invalid(format!("unexpected router argument {other:?}"))),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------- create

/// Creates a payment intent: the actor's covenant UTXO (a genesis authorised by the first funding
/// input) at output 0 and, for TokenToKas / TokenSwap, the locked token A owned by the intent's
/// covenant id at output 1. The payer signs every input once (SIGHASH_ALL); the payload records (the
/// x402 commitment) are covered by every signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIntent {
    /// Router actor (`KasToToken_buy`, `TokenToKas_sell2`, ...): the fill shape the keeper must use.
    pub actor: String,
    pub state: IntentState,
    /// KAS of the intent UTXO. KasToToken: what the asks, the merchant carrier, fillers and the fee
    /// are paid from (the rest returns to the payer at execution). Token intents: what the keeper may
    /// spend on fillers and the fee besides the proceeds.
    #[serde(with = "crate::json::field")]
    pub value: u64,
    /// Token intents: the payer's key-owned token A UTXOs.
    #[serde(default)]
    pub tokens: Vec<TokenUtxo>,
    /// Token intents: units locked in the intent (at least `max_sell`).
    #[serde(with = "crate::json::field", default)]
    pub lock_amount: i64,
    /// Token intents: KAS on the locked token UTXO (it becomes the carrier of the payer's token change).
    #[serde(with = "crate::json::field", default)]
    pub lock_carrier: u64,
    /// KAS on the payer's token change output, if the inputs hold more than `lock_amount`.
    #[serde(with = "crate::json::field", default)]
    pub token_change_carrier: u64,
    /// The payer's P2PK UTXOs; the first authorises the genesis.
    pub funding: Vec<KeyUtxo>,
    /// KAS change key (default: the first funding key).
    #[serde(with = "crate::json::field", default)]
    pub change: Option<[u8; 32]>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Builds an intent creation transaction.
pub fn build_create_intent(r: &CreateIntent, budgets: BudgetFn) -> Result<BuiltTx> {
    let actor = actor_of(&r.actor)?;
    r.state.check()?;
    r.state.check_actor(actor)?;
    let spk = r.state.spk(actor)?;
    if r.funding.is_empty() {
        return invalid("creating an intent needs a P2PK funding input (it authorises the genesis)");
    }
    pos(r.value as i64, "intent value")?;
    if r.value < MIN_INTENT_VALUE {
        return invalid(format!(
            "the intent holds {} sompi, below MIN_INTENT_VALUE ({MIN_INTENT_VALUE}): its cancel and its expiry could not pay their fee",
            r.value
        ));
    }
    let change = r.change.unwrap_or(r.funding[0].pubkey);
    let mut d = Draft::new(0, &r.fee, Some(change))?;
    let locked = locked_token(&r.state);
    let mut ext = None;
    if let Some(token) = locked {
        if r.tokens.is_empty() {
            return invalid("a token intent needs the payer's token inputs");
        }
        for t in &r.tokens {
            check_token_input(t, token)?;
            if !t.state.is_user() {
                return invalid("intent tokens must come from key-owned UTXOs (owner scheme 0x00 / KRON address presence)");
            }
            if *ext.get_or_insert(t.state.extension()) != t.state.extension() {
                return invalid("token inputs mix extension commitments");
            }
            d.add_token_input(t, token, token_witness(&t.state)?)?;
        }
    } else if !r.tokens.is_empty() {
        return invalid("a KasToToken intent locks KAS only");
    }
    let auth = d.inputs.len();
    for f in &r.funding {
        d.add_p2pk(f);
    }
    let at = d.reserve_output();
    debug_assert_eq!(at, INTENT_OUTPUT as usize);
    let id = d.fill_genesis(at, auth, r.value, spk, None)?;
    if let Some(token) = locked {
        let fam = token.1.family();
        let ext = ext.expect("token inputs");
        let max_sell = r.state.max_sell().expect("token intent");
        if r.lock_amount < max_sell {
            return invalid(format!("the intent may sell {max_sell} units but locks only {}", r.lock_amount));
        }
        // a KRON output holds 1..=1e9 units: the payer's change of an execution must not be empty
        if fam == Family::Kron && r.lock_amount <= max_sell {
            return invalid(format!(
                "a KRON intent locks more than it may sell (its token change after a sale of max_sell {max_sell} must hold a unit)"
            ));
        }
        // the state pins the lock this transaction creates (its amount and extension commitment)
        if r.state.lock_pin() != Some((r.lock_amount, ext)) {
            return invalid(format!(
                "the intent's lock pin (lock_amount, lock_extension) must be the lock it creates: {} units, extension {}",
                r.lock_amount,
                crate::json::to_hex(&ext)
            ));
        }
        let have: i64 = r.tokens.iter().map(|t| t.state.amount()).sum();
        if have < r.lock_amount {
            return invalid(format!("token inputs hold {have} < the {} units to lock", r.lock_amount));
        }
        let o =
            d.add_token_output(token, TokenState::custody(fam, r.lock_amount, id, ext), pos(r.lock_carrier as i64, "lockCarrier")?)?;
        debug_assert_eq!(o, LOCK_OUTPUT as usize);
        if have > r.lock_amount {
            let owner = r.tokens[0].state.owner();
            d.add_token_output(
                token,
                TokenState::user(fam, have - r.lock_amount, owner, ext),
                pos(r.token_change_carrier as i64, "tokenChangeCarrier")?,
            )?;
        }
    }
    if !r.records.is_empty() {
        d.payload = payload::encode(&r.records)?;
    }
    d.seal(budgets)
}

// ---------------------------------------------------------------------------------------------- cancel

/// The payer cancels an intent (SIGHASH_ALL by the intent's payer key) and, for a token intent,
/// takes the locked tokens back in the same transaction (the intent's presence authorises them). The
/// router requires the lock at input 1, as the one token input of its token (after the cancel nothing could move
/// tokens owned by the intent id), and the builder refuses a token intent's cancel without it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelIntent {
    pub actor: String,
    pub state: IntentState,
    /// The intent UTXO (its covenant id is the intent id).
    pub intent: Utxo,
    /// Token intents: the locked token UTXO (owned by the intent id). Required for them.
    #[serde(default)]
    pub lock: Option<TokenUtxo>,
    /// Receiver of the KAS and the tokens (default: the intent's payer).
    #[serde(with = "crate::json::field", default)]
    pub to: Option<[u8; 32]>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Builds the payer's cancel.
pub fn build_cancel_intent(r: &CancelIntent, budgets: BudgetFn) -> Result<BuiltTx> {
    let actor = actor_of(&r.actor)?;
    let id = r.intent.covenant_id.ok_or_else(|| Error::Invalid("the intent UTXO has no covenant id".into()))?;
    let payer = r.state.payer();
    let to = r.to.unwrap_or(payer);
    check_key(&to, "cancel receiver")?;
    let mut d = Draft::new(0, &r.fee, Some(to))?;
    let mut args = vec![Arg::Sig(payer)];
    args.extend(int_args(cancel_args(actor, &r.state)?)?);
    let plan = SigPlan::Router { actor: actor.name.into(), state: r.state.encode(actor)?, entry: "cancel".into(), args };
    d.add_input(&r.intent, plan, router_role(actor, "cancel"), SEQUENCE_NONFINAL);
    match (locked_token(&r.state), &r.lock) {
        (Some(token), Some(l)) => {
            check_token_input(l, token)?;
            if l.state.owner() != id || !l.state.is_covenant_owned() {
                return invalid("the locked token UTXO is not owned by this intent");
            }
            check_lock_pin(&r.state, l)?;
            d.add_token_input(l, token, Witness::CovenantId)?;
            let back = TokenState::user(token.1.family(), l.state.amount(), to, l.state.extension());
            d.add_token_output(token, back, l.utxo.amount)?;
        }
        // The cancel ends the intent covenant: the locked tokens (owned by the intent id) can never move afterwards.
        (Some(_), None) => return invalid("a token intent's cancel returns its locked tokens (the lock UTXO is required)"),
        (None, Some(_)) => return invalid("a KasToToken intent locks no tokens"),
        (None, None) => {}
    }
    d.seal(budgets)
}

// ---------------------------------------------------------------------------------------------- expire

/// Anyone expires an intent from its deadline on (no signature): the intent's KAS back to the payer at
/// output 0 (less the fee, at most [`EXPIRE_MAX_FEE`]) and, for a token intent, the locked tokens whole to
/// the payer's key at output 1. The transaction's lock time is the deadline (a unix-ms time lock), so a
/// node accepts it once its past median time reached the deadline.
///
/// ```text
///   in   [0] intent.expire  [1] locked token A (owned by the intent; token intents)
///   out  [0] payer KAS  [1] payer token A (token intents)
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpireIntent {
    pub actor: String,
    pub state: IntentState,
    /// The intent UTXO (its covenant id is the intent id).
    pub intent: Utxo,
    /// Token intents: the locked token UTXO (owned by the intent id).
    #[serde(default)]
    pub lock: Option<TokenUtxo>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// Builds an expiry. Fails if the fee would exceed [`EXPIRE_MAX_FEE`] (the router would reject it).
pub fn build_expire_intent(r: &ExpireIntent, budgets: BudgetFn) -> Result<BuiltTx> {
    let actor = actor_of(&r.actor)?;
    r.state.check()?;
    let id = r.intent.covenant_id.ok_or_else(|| Error::Invalid("the intent UTXO has no covenant id".into()))?;
    let payer = r.state.payer();
    let mut d = Draft::new(r.state.deadline() as u64, &r.fee, Some(payer))?;
    let j = d.reserve_output();
    let args = int_args(expire_args(actor, &r.state)?)?;
    let plan = SigPlan::Router { actor: actor.name.into(), state: r.state.encode(actor)?, entry: "expire".into(), args };
    let me = d.add_input(&r.intent, plan, router_role(actor, "expire"), SEQUENCE_NONFINAL);
    debug_assert_eq!((me, j), (0, 0));
    match (locked_token(&r.state), &r.lock) {
        (Some(token), Some(l)) => {
            check_token_input(l, token)?;
            if l.state.owner() != id || !l.state.is_covenant_owned() {
                return invalid("the locked token UTXO is not owned by this intent");
            }
            check_lock_pin(&r.state, l)?;
            d.add_token_input(l, token, Witness::CovenantId)?;
            let back = TokenState::user(token.1.family(), l.state.amount(), payer, l.state.extension());
            d.add_token_output(token, back, l.utxo.amount)?;
        }
        (Some(_), None) => return invalid("a token intent's expiry returns its locked tokens"),
        (None, Some(_)) => return invalid("a KasToToken intent locks no tokens"),
        (None, None) => {}
    }
    let built = d.seal(budgets)?;
    if built.fee.change_output != Some(0) {
        return invalid("the intent's KAS does not cover the expiry's fee (the payer's output 0 is missing)");
    }
    let (tx, _) = built.tx.to_tx()?;
    if tx.outputs[0].value + EXPIRE_MAX_FEE < r.intent.amount {
        return invalid(format!("the expiry's fee exceeds EXPIRE_MAX_FEE ({EXPIRE_MAX_FEE} sompi)"));
    }
    Ok(built)
}

// ---------------------------------------------------------------------------------------------- execute

/// One ask an execution buys from: `amount` base units out of its exact custody.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentAsk {
    pub order: OrderUtxo<AskState>,
    pub custody: TokenUtxo,
    #[serde(with = "crate::json::field")]
    pub amount: i64,
}

/// One bid an execution sells `amount` base units into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentBid {
    pub order: OrderUtxo<BidState>,
    #[serde(with = "crate::json::field")]
    pub amount: i64,
}

/// A keeper's execution of an intent against named orders (no signature: every input is a covenant).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteIntent {
    pub actor: String,
    pub state: IntentState,
    /// The intent UTXO.
    pub intent: Utxo,
    /// Token intents: the locked token UTXO.
    #[serde(default)]
    pub lock: Option<TokenUtxo>,
    /// Asks (KasToToken, TokenSwap), in transaction order: every one but the last is sold out.
    #[serde(default)]
    pub asks: Vec<IntentAsk>,
    /// Bids (TokenToKas, TokenSwap), in transaction order: every one but the last ends.
    #[serde(default)]
    pub bids: Vec<IntentBid>,
    /// KAS on the merchant's token delivery (KasToToken, TokenSwap).
    #[serde(with = "crate::json::field", default)]
    pub merchant_carrier: u64,
    /// KAS the merchant receives (TokenToKas; at least the intent's `merchant_kas`).
    #[serde(with = "crate::json::field", default)]
    pub merchant_kas: u64,
    /// KAS on the payer's token A change (token intents; default the locked UTXO's carrier).
    #[serde(with = "crate::json::field", default)]
    pub payer_token_carrier: Option<u64>,
    /// Value of a filler output (holds an output index open; paid to its key).
    #[serde(with = "crate::json::field")]
    pub filler: u64,
    /// Keeper key: receives the residue (token intents) and the fillers.
    #[serde(with = "crate::json::field")]
    pub keeper: [u8; 32],
    #[serde(with = "crate::json::field", default)]
    pub lock_time: u64,
    /// Payload records of the execution (optional).
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub fee: FeeOptions,
}

/// What an execution does, for the keeper's accounting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionFacts {
    /// Index of the merchant output.
    pub merchant_output: u32,
    /// Token units bought from the asks.
    #[serde(with = "crate::json::field")]
    pub bought: i64,
    /// Token units sold into the bids.
    #[serde(with = "crate::json::field")]
    pub sold: i64,
    /// What the asks may demand at their quotes (the router's `pay`).
    #[serde(with = "crate::json::field")]
    pub quote_pay: i64,
    /// KAS paid to the ask makers (all-in, without the carriers of ended asks).
    #[serde(with = "crate::json::field")]
    pub ask_all_in: i64,
    /// KAS released by the bids (all-in).
    #[serde(with = "crate::json::field")]
    pub bid_all_in: i64,
}

/// An ask leg's outputs and plan.
struct AskPlan {
    payout: u64,
    rest: Option<(u64, TokenState, u64, kaspa_consensus_core::tx::ScriptPublicKey)>, // custody carrier, rest state, ask carrier, continuation spk
    take: i64,
    quote: i64,
    all_in: i64,
    rests: bool,
}

fn plan_ask(a: &IntentAsk, token: Token, lock: i64) -> Result<AskPlan> {
    let s = &a.order.state;
    let cov = a.order.utxo.covenant_id.ok_or_else(|| Error::Invalid("ask UTXO has no covenant id".into()))?;
    if s.token_cov_id != token.0 {
        return invalid("an ask of another token");
    }
    let p = super::token_program(&s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len)?;
    if p != token.1 {
        return invalid(format!("an ask of the {} program; the intent buys {} tokens", p.name(), token.1.name()));
    }
    check_custody(&a.custody, Some(cov), token, s.custody_amount())?;
    if s.slope != 0 || s.interval != 0 {
        return invalid("intent executions take plain asks only (no auction, no TWAP)");
    }
    if lock < s.active_from {
        return invalid("ask not active yet");
    }
    let n = a.amount;
    pos(n, "ask amount")?;
    if !s.fill_ok(n) {
        return invalid(format!("an ask fill of {n} base units: n <= amountLeft, the minimum fill, maxFill"));
    }
    let all_in = super::need(s.proceeds(n, 0, a.order.utxo.block_daa_score as i64), "ask proceeds")?;
    let quote = super::need(quote_of(n, s.price, s.scale, Round::Up), "ask quote")?;
    let rest = s.amount_left - n;
    let rests = rest > 0;
    if rests && s.tif != TIF_GTC {
        return invalid("only a GTC ask can rest; an IOC / FOK ask in an intent is sold out");
    }
    let (payout, rest_out) = if rests {
        let next = AskState { amount_left: rest, ..s.clone() };
        (
            pos(all_in, "ask payout")?,
            Some((a.custody.utxo.amount, a.custody.state.with_amount(rest), a.order.utxo.amount, next.spk_for(Family::Kcc20))),
        )
    } else {
        (pos(all_in + a.order.utxo.amount as i64 + a.custody.utxo.amount as i64, "ask payout")?, None)
    };
    Ok(AskPlan { payout, rest: rest_out, take: n, quote, all_in, rests })
}

struct BidPlan {
    delivery: u64,
    cont: Option<(u64, kaspa_consensus_core::tx::ScriptPublicKey)>,
    sold: i64,
    all_in: i64,
    rests: bool,
    state: TokenState,
}

fn plan_bid(b: &IntentBid, token: Token, lock: i64) -> Result<BidPlan> {
    let s = &b.order.state;
    if s.token_cov_id != token.0 {
        return invalid("a bid of another token");
    }
    let p = super::token_program(&s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len)?;
    if p != token.1 {
        return invalid(format!("a bid of the {} program; the intent sells {} tokens", p.name(), token.1.name()));
    }
    let fam = p.family();
    if fam == Family::Kron && s.extension_commitment != [0; 32] {
        return invalid("a KRON bid has no extension commitment");
    }
    if s.slope != 0 || s.interval != 0 {
        return invalid("intent executions take plain bids only (no rising price, no DCA)");
    }
    if lock < s.active_from {
        return invalid("bid not active yet");
    }
    let n = b.amount;
    pos(n, "bid amount")?;
    let v = b.order.utxo.amount as i64;
    if !s.fill_ok(n, v) {
        return invalid(format!("a bid fill of {n} base units: maxFill, the budget, the minimum fill"));
    }
    let used = super::need(s.used(n), "bid budget")?;
    let all_in = super::need(s.spend(n, 0, b.order.utxo.block_daa_score as i64), "bid spend")?;
    let left = v - used;
    let can_continue = s.can_continue(left);
    let rests = s.tif == TIF_GTC && can_continue;
    if s.tif == TIF_FOK && can_continue {
        return invalid("a FOK bid must be filled completely");
    }
    let (delivery, cont) = if rests {
        (s.delivery_carrier + used - all_in, Some((pos(left - s.delivery_carrier, "bid continuation")?, s.spk_for(fam))))
    } else {
        (v - all_in, None)
    };
    Ok(BidPlan {
        delivery: pos(delivery, "bid delivery")?,
        cont,
        sold: n,
        all_in,
        rests,
        state: TokenState::user(fam, n, s.maker, s.extension_commitment),
    })
}

fn check_shape(actor: &Actor, asks: &[AskPlan], bids: &[BidPlan]) -> Result<()> {
    let s = actor.shape;
    if asks.len() != s.asks || bids.len() != s.bids {
        return invalid(format!("{} fills {} asks and {} bids", actor.name, s.asks, s.bids));
    }
    for (i, a) in asks.iter().enumerate() {
        let want = i + 1 == asks.len() && s.last_ask_rests;
        if a.rests != want {
            return invalid(format!(
                "{}: ask {} must {}",
                actor.name,
                i + 1,
                if want { "rest (GTC, an amount left)" } else { "be sold out" }
            ));
        }
    }
    for (i, b) in bids.iter().enumerate() {
        let want = i + 1 == bids.len() && s.last_bid_rests;
        if b.rests != want {
            return invalid(format!(
                "{}: bid {} must {}",
                actor.name,
                i + 1,
                if want { "continue (GTC)" } else { "end (exhausted / IOC / FOK)" }
            ));
        }
    }
    Ok(())
}

fn order_entry(id: TemplateId, state: Vec<u8>, name: &str, args: Vec<Arg>) -> SigPlan {
    SigPlan::Entry { template: id, state, entry: name.to_string(), args }
}

/// Builds an execution and returns it with its facts.
pub fn build_execute_intent(r: &ExecuteIntent, budgets: BudgetFn) -> Result<(BuiltTx, ExecutionFacts)> {
    let actor = actor_of(&r.actor)?;
    r.state.check()?;
    let istate = r.state.encode(actor)?;
    let id = r.intent.covenant_id.ok_or_else(|| Error::Invalid("the intent UTXO has no covenant id".into()))?;
    check_key(&r.keeper, "keeper key")?;
    let fam_a = r.state.a_family();
    let lock = r.lock_time as i64;
    let kind = actor.shape.kind;
    let merchant = r.state.merchant();
    let payer = r.state.payer();
    let filler = pos(r.filler as i64, "filler")?;
    let tok_b = merchant_token(&r.state);
    let tok_a = locked_token(&r.state);
    let no_token = || Error::Invalid(format!("{} trades no such token", actor.name));
    let asks: Vec<AskPlan> = r.asks.iter().map(|a| plan_ask(a, tok_b.ok_or_else(no_token)?, lock)).collect::<Result<_>>()?;
    let bids: Vec<BidPlan> = r.bids.iter().map(|b| plan_bid(b, tok_a.ok_or_else(no_token)?, lock)).collect::<Result<_>>()?;
    check_shape(actor, &asks, &bids)?;
    if r.asks.iter().any(|a| a.custody.state.extension() != r.asks[0].custody.state.extension()) {
        return invalid("the asks' custodies mix extension commitments");
    }
    // the B pin (router_head.ag, "B PIN"): every ask escrow carries the extension commitment the intent names
    if let Some(ext) = r.state.b_extension() {
        if r.asks.iter().any(|a| a.custody.state.extension() != ext) {
            return invalid(format!(
                "an ask's custody carries another extension commitment than the intent's b_extension {}",
                crate::json::to_hex(&ext)
            ));
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for u in r.asks.iter().map(|a| &a.order.utxo).chain(r.bids.iter().map(|b| &b.order.utxo)) {
        if !ids.insert(u.covenant_id) {
            return invalid("an order appears twice in one execution");
        }
    }
    let mut facts = ExecutionFacts {
        bought: asks.iter().map(|a| a.take).sum(),
        sold: bids.iter().map(|b| b.sold).sum(),
        quote_pay: asks.iter().map(|a| a.quote).sum(),
        ask_all_in: asks.iter().map(|a| a.all_in).sum(),
        bid_all_in: bids.iter().map(|b| b.all_in).sum(),
        merchant_output: 0,
    };
    // The router's own bounds, checked here so a built execution never fails on them.
    match &r.state {
        IntentState::KasToToken { amount, max_pay, .. } => {
            if facts.bought != *amount {
                return invalid(format!("the asks deliver {} units, the merchant must receive exactly {amount}", facts.bought));
            }
            if facts.quote_pay > *max_pay {
                return invalid(format!("the asks quote {} sompi, above the intent's max_pay {max_pay}", facts.quote_pay));
            }
        }
        IntentState::TokenToKas { merchant_kas, max_sell, .. } => {
            if facts.sold > *max_sell {
                return invalid(format!("selling {} units exceeds the intent's max_sell {max_sell}", facts.sold));
            }
            if (r.merchant_kas as i64) < *merchant_kas {
                return invalid("the merchant must receive at least the intent's merchant_kas");
            }
        }
        IntentState::TokenSwap { max_sell_a, amount_b, .. } => {
            if facts.sold > *max_sell_a {
                return invalid(format!("selling {} units exceeds the intent's max_sell_a {max_sell_a}", facts.sold));
            }
            if facts.bought != *amount_b {
                return invalid(format!("the asks deliver {} units, the merchant must receive exactly {amount_b}", facts.bought));
            }
        }
    }
    let lock_tok = match (tok_a, &r.lock) {
        (Some(t), Some(l)) => {
            check_token_input(l, t)?;
            if l.state.owner() != id || !l.state.is_covenant_owned() {
                return invalid("the locked token UTXO is not owned by this intent");
            }
            check_lock_pin(&r.state, l)?;
            if l.state.amount() < facts.sold {
                return invalid(format!("the intent locks {} units, the execution sells {}", l.state.amount(), facts.sold));
            }
            // a KRON output holds at least one unit: the payer's change must not be empty
            if fam_a == Family::Kron && l.state.amount() == facts.sold {
                return invalid("a KRON execution leaves the payer at least one unit (the token program refuses an empty output)");
            }
            Some(l)
        }
        (Some(_), None) => return invalid("a token intent's execution spends its locked tokens"),
        (None, Some(_)) => return invalid("a KasToToken intent locks no tokens"),
        (None, None) => None,
    };

    // KasToToken: the change (the payer's) goes to the reserved slot j; token intents: the keeper's, last.
    let change = if kind == IntentKind::KasToToken { payer } else { r.keeper };
    let mut d = Draft::new(r.lock_time, &r.fee, Some(change))?;
    let ka = r.asks.len();
    let kb = r.bids.len();
    let a0 = kb + ka; // first token input
    let custody_base = if kind == IntentKind::TokenSwap { a0 + 1 } else { a0 };
    let bid_kind = TemplateId::KobBid.in_family(fam_a);
    let prog_a = tok_a.map(|t| t.1.name()).unwrap_or_default();
    let prog_b = tok_b.map(|t| t.1.name()).unwrap_or_default();

    // inputs: bids, asks
    for (i, b) in r.bids.iter().enumerate() {
        let s = &b.order.state;
        let first_tok = kb + ka; // token A (the locked UTXO) follows the order inputs
        let role = format!("{}.fill.{}@{prog_a}", bid_kind.name(), if bids[i].rests { "cont" } else { "close" });
        d.add_input(
            &b.order.utxo,
            order_entry(bid_kind, s.encode_for(fam_a), "fill", vec![nb(b.amount), Arg::Int(first_tok as i64), Arg::Int(0)]),
            role,
            SEQUENCE_NONFINAL,
        );
    }
    for (m, a) in r.asks.iter().enumerate() {
        let s = &a.order.state;
        let role = format!("KobAsk.settle.{}@{prog_b}", if asks[m].rests { "rest" } else { "close" });
        // tokOut: the custody rest (first output after the positional / payout slots); patched below
        d.add_input(
            &a.order.utxo,
            order_entry(
                TemplateId::KobAsk,
                s.encode_for(Family::Kcc20),
                "settle",
                vec![nb(a.amount), Arg::Int((custody_base + m) as i64), Arg::Int(0), Arg::Int(0)],
            ),
            role,
            SEQUENCE_NONFINAL,
        );
    }
    // token inputs: the locked token A (token intents), then the custodies of B
    if let Some(l) = lock_tok {
        d.add_token_input(l, tok_a.expect("token intent"), Witness::CovenantId)?;
    }
    for a in &r.asks {
        d.add_token_input(&a.custody, tok_b.expect("ask legs buy the merchant token"), Witness::CovenantId)?;
    }
    let fill = FillArgs {
        asks: r.asks.iter().map(|a| a.order.utxo.covenant_id.unwrap_or_default()).collect(),
        bids: r.bids.iter().map(|b| b.order.utxo.covenant_id.unwrap_or_default()).collect(),
        bid_amounts: r.bids.iter().map(|b| b.amount).collect(),
    };
    let args: Vec<Arg> = fill_args(actor, &r.state, &fill)?
        .into_iter()
        .map(|v| match v {
            silverscript_abi::ArtifactValue::Int(i) => Ok(Arg::Int(i)),
            silverscript_abi::ArtifactValue::Bytes(b) => Ok(Arg::Bytes(b)),
            other => Err(Error::Invalid(format!("unexpected router argument {other:?}"))),
        })
        .collect::<Result<_>>()?;
    let me = d.add_input(
        &r.intent,
        SigPlan::Router { actor: actor.name.into(), state: istate, entry: actor.entry.into(), args },
        router_role(actor, actor.entry),
        SEQUENCE_NONFINAL,
    );
    let p2pk_keeper = p2pk_spk(&r.keeper);
    let last_ask = r.asks.len().checked_sub(1);
    let last_bid = r.bids.len().checked_sub(1);
    let mut tok_out_patch: Option<(usize, usize)> = None; // (ask input, output) of the custody rest

    match kind {
        IntentKind::KasToToken => {
            let tok = tok_b.expect("KasToToken buys a token");
            for (m, a) in asks.iter().enumerate() {
                d.add_output(a.payout, p2pk_spk(&r.asks[m].order.state.maker), None);
            }
            if let Some((carrier, st, _, _)) = last_ask.and_then(|i| asks[i].rest.clone()) {
                let o = d.add_token_output(tok, st, carrier)?;
                tok_out_patch = Some((kb + last_ask.unwrap(), o));
            }
            while d.outputs.len() < me {
                d.add_output(filler, p2pk_spk(&payer), None);
            }
            let j = d.reserve_output();
            debug_assert_eq!(j, me);
            let ext = r.asks[0].custody.state.extension();
            let mo = d.add_token_output(
                tok,
                TokenState::user(Family::Kcc20, facts.bought, merchant, ext),
                pos(r.merchant_carrier as i64, "merchant carrier")?,
            )?;
            debug_assert_eq!(mo, me + 1);
            facts.merchant_output = mo as u32;
            if let Some((_, _, ask_carrier, spk)) = last_ask.and_then(|i| asks[i].rest.clone()) {
                let i = last_ask.unwrap();
                d.add_output(ask_carrier, spk, Some(((kb + i) as u16, r.asks[i].order.utxo.covenant_id.expect("checked"))));
            }
        }
        IntentKind::TokenToKas => {
            let tok = tok_a.expect("TokenToKas sells a token");
            let l = lock_tok.expect("checked");
            for b in &bids {
                d.add_token_output(tok, b.state.clone(), b.delivery)?;
            }
            match last_bid.and_then(|i| bids[i].cont.clone()) {
                Some((v, spk)) => {
                    let i = last_bid.unwrap();
                    d.add_output(v, spk, Some((i as u16, r.bids[i].order.utxo.covenant_id.expect("bid UTXO has a covenant id"))));
                }
                None => {
                    d.add_output(filler, p2pk_keeper.clone(), None);
                }
            }
            let mo = d.add_output(pos(r.merchant_kas as i64, "merchant KAS")?, p2pk_spk(&merchant), None);
            if mo != me {
                return Err(Error::Invalid(format!("layout: merchant output at {mo}, intent at input {me}")));
            }
            facts.merchant_output = mo as u32;
            d.add_token_output(
                tok,
                TokenState::user(fam_a, l.state.amount() - facts.sold, payer, l.state.extension()),
                pos(r.payer_token_carrier.unwrap_or(l.utxo.amount) as i64, "payer token carrier")?,
            )?;
        }
        IntentKind::TokenSwap => {
            let ta = tok_a.expect("swap sells token A");
            let tb = tok_b.expect("swap buys token B");
            let l = lock_tok.expect("checked");
            for b in &bids {
                d.add_token_output(ta, b.state.clone(), b.delivery)?;
            }
            for (m, a) in asks.iter().enumerate() {
                d.add_output(a.payout, p2pk_spk(&r.asks[m].order.state.maker), None);
            }
            // the queue: custody rest of B, bid continuation, ask continuation
            enum Q {
                Tok(u64, TokenState),
                Plain(u64, kaspa_consensus_core::tx::ScriptPublicKey, Option<(u16, [u8; 32])>),
            }
            let mut queue = vec![];
            let ask_rest = last_ask.and_then(|i| asks[i].rest.clone());
            if let Some((carrier, st, _, _)) = &ask_rest {
                queue.push(Q::Tok(*carrier, st.clone()));
            }
            if let Some((v, spk)) = last_bid.and_then(|i| bids[i].cont.clone()) {
                let i = last_bid.unwrap();
                queue.push(Q::Plain(v, spk, Some((i as u16, r.bids[i].order.utxo.covenant_id.expect("bid UTXO has a covenant id")))));
            }
            if let Some((_, _, carrier, spk)) = &ask_rest {
                let i = last_ask.unwrap();
                queue.push(Q::Plain(
                    *carrier,
                    spk.clone(),
                    Some(((kb + i) as u16, r.asks[i].order.utxo.covenant_id.expect("checked"))),
                ));
            }
            let mut queue = queue.into_iter();
            let put = |d: &mut Draft, q: Q, patch: &mut Option<(usize, usize)>| -> Result<()> {
                match q {
                    Q::Tok(v, st) => {
                        let o = d.add_token_output(tb, st, v)?;
                        *patch = Some((kb + last_ask.expect("an ask rests"), o));
                    }
                    Q::Plain(v, spk, cov) => {
                        d.add_output(v, spk, cov);
                    }
                }
                Ok(())
            };
            for _ in 0..ka + 1 {
                match queue.next() {
                    Some(q) => put(&mut d, q, &mut tok_out_patch)?,
                    None => {
                        d.add_output(filler, p2pk_keeper.clone(), None);
                    }
                }
            }
            let ext_b = r.asks[0].custody.state.extension();
            let mo = d.add_token_output(
                tb,
                TokenState::user(Family::Kcc20, facts.bought, merchant, ext_b),
                pos(r.merchant_carrier as i64, "merchant carrier")?,
            )?;
            if mo != me {
                return Err(Error::Invalid(format!("layout: merchant output at {mo}, intent at input {me}")));
            }
            facts.merchant_output = mo as u32;
            d.add_token_output(
                ta,
                TokenState::user(fam_a, l.state.amount() - facts.sold, payer, l.state.extension()),
                pos(r.payer_token_carrier.unwrap_or(l.utxo.amount) as i64, "payer token carrier")?,
            )?;
            for q in queue {
                put(&mut d, q, &mut tok_out_patch)?;
            }
        }
    }
    if let Some((input, out)) = tok_out_patch {
        if let crate::tx::DPlan::Plain(SigPlan::Entry { args, .. }) = &mut d.inputs[input].plan {
            args[2] = Arg::Int(out as i64);
        }
    }
    if !r.records.is_empty() {
        d.payload = payload::encode(&r.records)?;
    }
    let built = d.seal(budgets)?;
    // KasToToken: the router's bound on the payer's change (out j + pay + max_extra >= the intent value)
    if let IntentState::KasToToken { max_extra, .. } = &r.state {
        let (tx, _) = built.tx.to_tx()?;
        let change = tx.outputs.get(me).map(|o| o.value as i64).unwrap_or(0);
        if built.fee.change_output != Some(me as u32) {
            return invalid("the payer's KAS change must be the intent's output j (the budget does not cover the execution)");
        }
        if change + facts.quote_pay + max_extra < r.intent.amount as i64 {
            return invalid(format!(
                "carriers, fillers and the fee exceed the intent's max_extra: the payer's change {change} + quote {} + max_extra {max_extra} < {}",
                facts.quote_pay, r.intent.amount
            ));
        }
    }
    Ok((built, facts))
}

// ---------------------------------------------------------------------------------------------- budgets

/// Budget of a router `cancel` input (one Schnorr signature check and, for a token intent, the read of its observed
/// lock under the token handle; measured by `tests/intent_builders.rs` over every actor and program, which fails if it is
/// too small or more than one unit too large).
pub const ROUTER_CANCEL_BUDGET: u16 = 16;
/// Budget of a router `expire` input (the largest over every actor: a token intent reads and pins its
/// locked tokens; measured by `tests/intent_builders.rs` like the cancel budget).
pub const ROUTER_EXPIRE_BUDGET: u16 = 13;
/// Budget a router fill input is first built with before `execute_intent` measures the exact one.
pub const ROUTER_FILL_BUDGET_FALLBACK: u16 = 900;

/// The committed budget table, plus the router roles it does not list (`router.<actor>.<entry>`).
pub fn intent_budgets(role: &str) -> Result<u16> {
    match crate::budget::lookup(role) {
        Ok(b) => Ok(b),
        Err(_) if role.starts_with("router.") && role.ends_with(".cancel") => Ok(ROUTER_CANCEL_BUDGET),
        Err(_) if role.starts_with("router.") && role.ends_with(".expire") => Ok(ROUTER_EXPIRE_BUDGET),
        Err(_) if role.starts_with("router.") => Ok(ROUTER_FILL_BUDGET_FALLBACK),
        Err(e) => Err(e),
    }
}

/// Builds an execution with the exact compute budget of every input (a first pass is measured in the
/// script engine, the second is built with the measured budgets, so the fee pays for what is used) and
/// finalises it: an execution needs no signature.
#[cfg(feature = "engine")]
pub fn execute_intent(r: &ExecuteIntent) -> Result<(crate::tx::SignedTx, ExecutionFacts)> {
    let (first, _) = build_execute_intent(r, &intent_budgets)?;
    let (tx, entries) = crate::tx::assemble(&first, &[])?;
    let units = crate::verify::measure_units(&tx, &entries)?;
    let mut by_role: std::collections::BTreeMap<String, u16> = std::collections::BTreeMap::new();
    for (role, u) in first.roles.iter().zip(units) {
        let b = crate::budget::budget_for_units(u);
        let e = by_role.entry(role.clone()).or_insert(0);
        *e = (*e).max(b);
    }
    let measured = |role: &str| by_role.get(role).copied().ok_or_else(|| Error::MissingBudget(role.to_string()));
    let (built, facts) = build_execute_intent(r, &measured)?;
    let signed = crate::tx::finalize(&built, &[], crate::tx::FinalizeOptions { tighten_budgets: true })?;
    Ok((signed, facts))
}
