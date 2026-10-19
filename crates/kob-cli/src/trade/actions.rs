//! `kob replace`, `kob cancel`, `kob refund`, `kob take` and `kob send`: actions on live orders and plain transfers, built
//! like the web wallet does (`web/src/kob/cancel.ts`, `web/src/ui/orders/amend.ts`) and like the keepers refund
//! (`kob-executor` keepers).

use kob_protocol::build::{
    build, Action, AmendOrder, Batch, CancelOrder, Leg, RefundOrder, Replacement, SendTokens, TokenRecipient, TokenRef,
};
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::json::to_hex;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::{AnyState, CondAskState, CondBidState};
use kob_protocol::tx::{BuiltTx, FeeOptions, KeyUtxo, OrderUtxo, TokenUtxo};
use serde_json::json;

use super::data::{self, Live};
use super::flow::{finish, Ready};
use super::market::{self, show_id, TokenMarket};
use super::opts::{parse, Common, Parsed, COMMON_HELP};
use super::plan::{
    select_tokens, slip_for_limit, stop_worst_price, usable_token_utxos, Issue, Severity, Side, MAX_IDLE_DAA, MAX_TOKEN_INPUTS,
};
use super::show::Units;
use super::units::{self, parse_kas, parse_kas_price, parse_token_amount, TokenUnit};
use crate::common::Signer;

const ONE_KAS: u64 = 100_000_000;
/// Headroom added to a reported shortfall when a funding UTXO is picked.
const FUNDING_HEADROOM: u64 = 500_000;
/// Most KAS a builder may fold into the maker's new order instead of a change output.
const TINY_CHANGE_MAX: u64 = 5 * ONE_KAS;
/// The lock time margin of a taker: the matchers' `now - 10 DAA`.
pub const LOCK_MARGIN_DAA: i64 = 10;

fn issue(code: &'static str, severity: Severity, message: impl Into<String>) -> Issue {
    Issue { code, severity, message: message.into() }
}

/// Builds with the order's own KAS first; on a shortfall adds the signer's UTXOs (the smallest one that covers it, else
/// the largest) and retries, at most 8 times (`cancel.ts` `buildWithFunding`).
fn build_with_funding(
    pool: &[KeyUtxo],
    initial: Vec<KeyUtxo>,
    make: impl Fn(Vec<KeyUtxo>) -> Action,
) -> Result<(BuiltTx, Vec<KeyUtxo>), String> {
    let mut pool: Vec<KeyUtxo> = pool.to_vec();
    pool.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
    let mut used = initial;
    for _ in 0..8 {
        match build(&make(used.clone())) {
            Ok(b) => return Ok((b, used)),
            Err(kob_protocol::Error::InsufficientFunds { need, have }) => {
                let deficit = need.saturating_sub(have) + FUNDING_HEADROOM;
                let rest: Vec<&KeyUtxo> = pool.iter().filter(|p| !used.contains(p)).collect();
                if rest.is_empty() {
                    return Err(format!(
                        "not enough KAS: {} short and no funding UTXO left",
                        units::kas((deficit - FUNDING_HEADROOM) as i128)
                    ));
                }
                let pick = rest.iter().rev().find(|p| p.utxo.amount >= deficit).copied().unwrap_or(rest[0]);
                used.push(pick.clone());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("could not fund the network fee within 8 attempts".into())
}

fn ctx(args: &[String], value_flags: &[&str], bool_flags: &[&str], usage: &str) -> Result<(Parsed, Common, Signer), i32> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{usage}{COMMON_HELP}");
        return Err(0);
    }
    let fail = |e: String| {
        eprintln!("error: {e}");
        2
    };
    let p = parse(args, value_flags, bool_flags).map_err(fail)?;
    let c = Common::from(&p).map_err(fail)?;
    let s = c.signer(args).map_err(fail)?;
    super::flow::check_network_first(&c).map_err(|e| {
        eprintln!("error: {e}");
        1
    })?;
    Ok((p, c, s))
}

fn cov_arg(s: &str) -> Result<[u8; 32], String> {
    kob_protocol::registry::parse_hex32(s).ok_or_else(|| format!("`{s}` is not a covenant id (64 hex)"))
}

/// The markets an order trades (its token, a pair order's two tokens), from the registry and the indexer.
fn markets_of(c: &Common, live: &Live) -> Result<Vec<TokenMarket>, String> {
    let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, c.indexer.as_deref())?;
    let toks: Vec<[u8; 32]> = match live.order.state.pair_tokens() {
        Some(p) => vec![p.a.cov_id, p.b.cov_id],
        None => vec![live.order.state.token_cov_id()],
    };
    // a token without a registry entry: the scale its listed orders use, else this order's own (its base token's)
    let (own_token, own_scale) = (live.order.state.token_cov_id(), live.order.state.scale());
    let order_scale = |cov: &[u8; 32]| -> Result<Option<i64>, String> {
        let listed = match c.indexer.as_deref() {
            Some(u) => data::order_scale(u, cov)?,
            None => None,
        };
        Ok(listed.or((*cov == own_token).then_some(own_scale)))
    };
    let remembered = |cov: &[u8; 32]| crate::recent::first_order_decimals(&c.network, cov);
    let declaration = |cov: &[u8; 32]| match c.indexer.as_deref() {
        Some(u) => data::declaration(u, cov),
        None => Ok(None),
    };
    let open = market::OpenToken {
        remembered: Some(&remembered),
        declaration: Some(&declaration),
        ..market::OpenToken::new(&c.network, &order_scale)
    };
    let mut ms: Vec<TokenMarket> =
        toks.iter().map(|t| market::resolve_with(&to_hex(t), &reg, &idx, Some(&open))).collect::<Result<_, _>>()?;
    // a commitment no source names: the one the order's own state (read from the node) carries for the token
    for m in ms.iter_mut().filter(|m| m.ext_source == market::ExtSource::Unknown) {
        let s = &live.order.state;
        let own = s.custody_ext(m.cov).or_else(|| (s.token_cov_id() == m.cov).then(|| s.extension_commitment()).flatten());
        if let Some(e) = own {
            m.ext = e;
            m.ext_source = market::ExtSource::Order;
        }
    }
    Ok(ms)
}

fn units_of(ms: &[TokenMarket]) -> Units {
    ms.iter().map(|m| (m.cov, m.unit())).collect()
}

fn order_lines(live: &Live, ms: &[TokenMarket]) -> Vec<String> {
    let s = &live.order.state;
    let mut v = vec![format!("order {} ({}), maker {}", to_hex(&live.cov), s.template_id().name(), show_id(&s.maker(), ""))];
    v.push(format!("on the order: {}", units::kas(live.order.utxo.amount as i128)));
    for c in live.custody.iter().chain(live.prefund.iter()) {
        let m = ms.iter().find(|m| Some(m.cov) == c.utxo.covenant_id);
        let u = m.map(|m| m.unit()).unwrap_or(TokenUnit { ticker: String::new(), decimals: 0 });
        v.push(format!("custody: {}", units::tokens(c.state.amount() as i128, &u)));
    }
    if !live.strays.is_empty() || live.foreign > 0 {
        v.push(format!("strays: {} of its own token(s), {} of other tokens", live.strays.len(), live.foreign));
    }
    v
}

// ------------------------------------------------------------------------------------------------ replace

/// Help of `kob replace`.
pub const REPLACE_USAGE: &str = "\
kob replace: amend a live order (in place where the protocol allows it, else cancel + new order in one transaction)

USAGE
  kob replace <COVENANT_ID> [--price P] [--amount AMOUNT] [--tip T] [--stop P] [--stop-limit P] [--add-kas KAS] [--renew]
  kob replace <COVENANT_ID> [...] [--take-profit P] [--exit-tip T] [--entry-stop P] [--trail-step P] [--trail-gap P]
                            [--carrier KAS] [--ack-no-guards]

  plain limit orders (KobAsk / KobBid): --price, --tip, --amount (a sell; a buy keeps its KAS budget: --add-kas tops it
  up), --renew (a fresh 90-day expiry). A sell that keeps its amount, or a buy, is amended IN PLACE: same covenant id,
  the custody stays where it is, the order's own KAS pays the fee. Otherwise the order is cancelled and the replacement
  placed in the same transaction (custody, strays and any top-up tokens move to the new order).
  conditional orders (stop / take-profit / OCO): --price (the take-profit), --stop, --stop-limit, --amount, --tip, --renew;
  always a cancel-replace, an armed stop restarts unarmed.
  trailing stops, if-done entries (IFD / IFO, stop entries) and every pair order (limits, TWAP / DCA, Dutch,
  conditionals, if-done entries; prices B per whole A): the order is re-planned from its own terms with the changes
  (--price: the limit / take-profit / entry price; --take-profit, --stop, --stop-limit, --exit-tip: the exit of an
  if-done entry; --trail-step / --trail-gap: the trail; --entry-stop: a stop entry) exactly as `kob place` plans it, and
  cancelled + replaced in one transaction. A GTC is re-placed as a fresh GTC, a dated order keeps its date, a day order
  stays a day order. Repeat entries and the exits they booked are not replaced (cancel the position instead).
";

/// What the user changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AmendInput {
    /// Limit / take-profit price.
    pub price: Option<i64>,
    /// Stop.
    pub stop: Option<i64>,
    /// Stop-limit.
    pub stop_limit: Option<i64>,
    /// Base units.
    pub amount: Option<i64>,
    /// Tip.
    pub tip: Option<i64>,
    /// KAS added to a bid.
    pub add_kas: u64,
    /// Fresh 90-day expiry.
    pub renew: bool,
}

/// The replacement state and the KAS it holds (`amend.ts` `buildReplacement`).
pub fn replacement(live: &Live, i: &AmendInput, now_daa: i64, tick: i64) -> Result<(AnyState, i64, Vec<String>), String> {
    let value = live.order.utxo.amount as i64;
    let mut notes = vec![];
    let on_tick = |what: &str, p: i64| -> Result<i64, String> {
        if p <= 0 {
            return Err(format!("{what} must be greater than zero"));
        }
        if tick > 1 && p % tick != 0 {
            return Err(format!("{what} must be a multiple of the tick ({tick} sompi per token)"));
        }
        Ok(p)
    };
    let expiry = |old: i64| if i.renew { now_daa + MAX_IDLE_DAA } else { old };
    if i.renew {
        notes.push("renewed: good for another 90 days from now".to_string());
    }
    match &live.order.state {
        s @ (AnyState::KobAsk(_) | AnyState::KobAskKron(_) | AnyState::KobBid(_) | AnyState::KobBidKron(_)) => {
            if i.stop.is_some() || i.stop_limit.is_some() {
                return Err("a limit order has no stop".into());
            }
            let mut n = s.clone();
            match &mut n {
                AnyState::KobAsk(x) | AnyState::KobAskKron(x) => {
                    if i.add_kas > 0 {
                        return Err("only a buy order takes --add-kas".into());
                    }
                    x.price = on_tick("the price", i.price.unwrap_or(x.price))?;
                    x.tip = i.tip.unwrap_or(x.tip);
                    if x.tip < 0 || x.tip >= x.price {
                        return Err("the tip must be at least zero and below the price".into());
                    }
                    x.amount_left = i.amount.unwrap_or(x.amount_left);
                    x.min_fill = x.min_fill.min(x.amount_left);
                    x.expiry_daa = expiry(x.expiry_daa);
                    Ok((n, value, notes))
                }
                AnyState::KobBid(x) | AnyState::KobBidKron(x) => {
                    if i.amount.is_some() {
                        return Err(
                            "a buy order keeps its KAS budget: the amount follows from the price (top it up with --add-kas)".into()
                        );
                    }
                    x.price = on_tick("the price", i.price.unwrap_or(x.price))?;
                    x.tip = i.tip.unwrap_or(x.tip);
                    if x.tip < 0 {
                        return Err("the tip cannot be negative".into());
                    }
                    x.expiry_daa = expiry(x.expiry_daa);
                    let v = value + i.add_kas as i64;
                    let min = x.escrow(x.min_fill, 1).ok_or("the new terms do not fit the protocol's numbers")?;
                    if v < min {
                        return Err(format!(
                            "the escrow does not fund one minimum fill at this price: add at least {} with --add-kas",
                            units::kas((min - v) as i128)
                        ));
                    }
                    notes.push(format!(
                        "the buy order can take up to {} base units with its {} escrow",
                        x.buying_power(v),
                        units::kas(v as i128)
                    ));
                    Ok((n, v, notes))
                }
                _ => unreachable!(),
            }
        }
        s @ (AnyState::KobCondAsk(_) | AnyState::KobCondAskKron(_) | AnyState::KobCondBid(_) | AnyState::KobCondBidKron(_)) => {
            if i.add_kas > 0 {
                return Err("--add-kas applies to a plain buy order".into());
            }
            let mut n = s.clone();
            macro_rules! legs {
                ($x:ident, $side:expr) => {{
                    if $x.trail_step > 0 {
                        return Err("a trailing stop cannot be amended: cancel it and place a new one".into());
                    }
                    if $x.parent != [0; 32] {
                        return Err("an exit of a position cannot be amended on its own".into());
                    }
                    let has_tp = $x.tp_price > 0;
                    let has_stop = $x.stop_price > 0;
                    if i.price.is_some() && !has_tp {
                        return Err("this order has no take-profit leg".into());
                    }
                    if (i.stop.is_some() || i.stop_limit.is_some()) && !has_stop {
                        return Err("this order has no stop leg".into());
                    }
                    if has_tp {
                        $x.tp_price = on_tick("the take-profit", i.price.unwrap_or($x.tp_price))?;
                    }
                    if has_stop {
                        $x.stop_price = on_tick("the stop", i.stop.unwrap_or($x.stop_price))?;
                    }
                    if has_tp
                        && has_stop
                        && (if $side == Side::Sell { $x.tp_price <= $x.stop_price } else { $x.tp_price >= $x.stop_price })
                    {
                        return Err("the take-profit must be on the profit side of the stop".into());
                    }
                    if let Some(lim) = i.stop_limit {
                        if if $side == Side::Sell { lim > $x.stop_price } else { lim < $x.stop_price } {
                            return Err("the stop-limit must be on the far side of the stop".into());
                        }
                        $x.slip_bps = slip_for_limit($side, $x.stop_price, lim);
                    }
                    if has_stop && stop_worst_price($side, $x.stop_price, $x.slip_bps) <= 0 {
                        return Err("the stop is too low for its price band".into());
                    }
                    $x.tip = i.tip.unwrap_or($x.tip);
                    $x.amount_left = i.amount.unwrap_or($x.amount_left);
                    $x.min_fill = $x.min_fill.min($x.amount_left);
                    $x.expiry_daa = expiry($x.expiry_daa);
                    $x.armed = 0;
                }};
            }
            match &mut n {
                AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => {
                    let x: &mut CondAskState = x;
                    legs!(x, Side::Sell);
                    if x.tip < 0
                        || x.tip
                            >= (if x.tp_price > 0 { x.tp_price } else { i64::MAX }).min(if x.stop_price > 0 {
                                stop_worst_price(Side::Sell, x.stop_price, x.slip_bps)
                            } else {
                                i64::MAX
                            })
                    {
                        return Err("the tip must be at least zero and below every leg price".into());
                    }
                    Ok((n, value, notes))
                }
                AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => {
                    let x: &mut CondBidState = x;
                    legs!(x, Side::Buy);
                    let spend = |st: &CondBidState| st.escrow(1).map(|e| e - st.delivery_carrier);
                    let old = match &live.order.state {
                        AnyState::KobCondBid(o) | AnyState::KobCondBidKron(o) => spend(o),
                        _ => None,
                    };
                    let (Some(o), Some(nw)) = (old, spend(x)) else {
                        return Err("the new terms do not fit the protocol's numbers".into());
                    };
                    let v = value + nw - o;
                    if v <= 0 {
                        return Err("the new terms leave no KAS for the order".into());
                    }
                    Ok((n, v, notes))
                }
                _ => unreachable!(),
            }
        }
        other => Err(format!("a {} cannot be amended here: cancel it and place a new order", other.template_id().name())),
    }
}

/// Whether the amendment can be in place (`cancel.ts` `amendsInPlace`).
pub fn amends_in_place(live: &Live, rep: &AnyState) -> bool {
    let old = &live.order.state;
    if old.template_id() != rep.template_id() || !live.strays.is_empty() || live.foreign > 0 {
        return false;
    }
    match (old, rep) {
        (AnyState::KobBid(_) | AnyState::KobBidKron(_), _) => true,
        (AnyState::KobAsk(a) | AnyState::KobAskKron(a), AnyState::KobAsk(b) | AnyState::KobAskKron(b)) => {
            live.custody.is_some() && a.amount_left == b.amount_left
        }
        _ => false,
    }
}

/// The amend / cancel-replace transaction of `live` to `rep` holding `value` (`planAmendInPlace` / `planCancelReplace`).
pub fn build_replace(
    live: &Live,
    rep: &AnyState,
    value: i64,
    funding: &[KeyUtxo],
    token_utxos: &[TokenUtxo],
    markets: &[TokenMarket],
    maker: &[u8; 32],
    fee: &FeeOptions,
    token_carrier: Option<u64>,
) -> Result<(BuiltTx, bool, Vec<String>), String> {
    let mut notes = vec![];
    if amends_in_place(live, rep) {
        let top_up = value as u64 > live.order.utxo.amount;
        let (built, used) = build_with_funding(funding, vec![], |f| {
            Action::AmendOrder(AmendOrder {
                order: live.order.clone(),
                amended: rep.clone(),
                value: (!f.is_empty() || top_up).then_some(value as u64),
                funding: f,
                change: None,
                lock_time: 0,
                deadline: live.deadline,
                records: vec![],
                fee: fee.clone(),
            })
        })?;
        let kept = built.tx.outputs.first().map(|o| o.value).unwrap_or(0);
        let base = if used.is_empty() { live.order.utxo.amount } else { value as u64 };
        if kept + built.fee.fee < base || kept > base + TINY_CHANGE_MAX {
            return Err(format!(
                "the built amend leaves {} on the order, not about {}",
                units::kas(kept as i128),
                units::kas(base as i128)
            ));
        }
        notes.push(if matches!(rep, AnyState::KobBid(_) | AnyState::KobBidKron(_)) {
            "amended in place: the buy order keeps its id and its KAS stays on the order".to_string()
        } else {
            "amended in place: the order keeps its id and its tokens stay in custody (no token transfer)".to_string()
        });
        return Ok((built, true, notes));
    }
    // the tokens the replacement must hold, from the old custody, the strays and (when short) free tokens
    let mut tokens: Vec<TokenUtxo> = vec![];
    let mut strays = live.strays.clone();
    for (token, need) in rep.custodies().into_iter().filter(|c| c.1 > 0) {
        let m = markets.iter().find(|m| m.cov == token).ok_or("unknown token of the replacement")?;
        let held: Vec<&TokenUtxo> =
            live.custody.iter().chain(live.prefund.iter()).filter(|c| c.utxo.covenant_id == Some(token)).collect();
        let ext = held.first().map(|h| h.state.extension()).unwrap_or(m.ext);
        let (same, other): (Vec<TokenUtxo>, Vec<TokenUtxo>) = strays
            .iter()
            .cloned()
            .partition(|s| s.utxo.covenant_id != Some(token) || m.family == kob_protocol::Family::Kron || s.state.extension() == ext);
        if !other.is_empty() {
            notes.push(format!("{} stray token UTXO(s) with another extension commitment stay behind", other.len()));
        }
        strays = same;
        let have: i64 = held.iter().map(|h| h.state.amount()).sum::<i64>()
            + strays.iter().filter(|s| s.utxo.covenant_id == Some(token)).map(|s| s.state.amount()).sum::<i64>();
        let inputs = held.len() + strays.iter().filter(|s| s.utxo.covenant_id == Some(token)).count();
        let slots = m.slots.0.min(MAX_TOKEN_INPUTS);
        if inputs > slots {
            return Err(format!(
                "the order has more stray token UTXOs than one transaction moves ({slots} inputs): sweep them first (kob order sweep)"
            ));
        }
        if need > have {
            let usable = usable_token_utxos(m, maker, token_utxos);
            let picked = select_tokens(&usable, need - have, slots - inputs, &m.name()).map_err(|e| e.message)?;
            notes.push(format!(
                "the replacement needs more tokens than the order holds: {} added",
                units::tokens(picked.iter().map(|t| t.state.amount() as i128).sum(), &m.unit())
            ));
            tokens.extend(picked);
        }
    }
    let first = live.custody.as_ref().or(live.prefund.as_ref());
    let carrier =
        token_carrier.or(first.map(|c| c.utxo.amount)).or(tokens.first().map(|t| t.utxo.amount)).unwrap_or(DEFAULT_ORDER_CARRIER);
    let needs = rep.custodies().iter().any(|c| c.1 > 0);
    let mut rep = rep.clone();
    let was_armed = match (&live.order.state, &mut rep) {
        (_, AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x)) => std::mem::replace(&mut x.armed, 0) != 0,
        (_, AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x)) => std::mem::replace(&mut x.armed, 0) != 0,
        _ => false,
    };
    let old_armed = match &live.order.state {
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.armed != 0,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.armed != 0,
        _ => false,
    };
    if was_armed || old_armed {
        notes.push("an amended stop starts unarmed and must trigger again".into());
    }
    let (built, _) = build_with_funding(funding, vec![], |f| {
        Action::CancelOrder(CancelOrder {
            order: live.order.clone(),
            custody: live.custody.clone(),
            prefund: live.prefund.clone(),
            strays: strays.clone(),
            foreign: vec![],
            tokens: tokens.clone(),
            funding: f,
            change: None,
            replace: Some(Replacement {
                order: rep.clone(),
                value: value as u64,
                token_carrier: needs.then_some(carrier),
                deadline: live.deadline,
            }),
            lock_time: 0,
            records: vec![],
            fee: fee.clone(),
        })
    })?;
    let extra = built.tx.outputs.first().map(|o| o.value as i128).unwrap_or(0) - value as i128;
    if !(0..=TINY_CHANGE_MAX as i128).contains(&extra) {
        return Err(format!("the built replacement holds {extra} sompi more than planned"));
    }
    if live.foreign > 0 {
        notes.push(format!("{} stray UTXO(s) of other tokens stay with the old covenant id (kob cancel returns them)", live.foreign));
    }
    notes.push("cancel + replace: the order gets a new covenant id".into());
    Ok((built, false, notes))
}

/// The value flags of `kob replace`.
const REPLACE_VALUE_FLAGS: &[&str] = &[
    "--price",
    "--amount",
    "--tip",
    "--stop",
    "--stop-limit",
    "--add-kas",
    "--take-profit",
    "--exit-tip",
    "--trail-step",
    "--trail-gap",
    "--entry-stop",
    "--carrier",
];

/// Run `kob replace`.
pub fn run_replace(args: &[String]) -> i32 {
    let (p, c, signer) = match ctx(args, REPLACE_VALUE_FLAGS, &["--renew", "--ack-no-guards"], REPLACE_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let r = (|| -> Result<(Ready, Units), String> {
        let [id] = p.pos.as_slice() else { return Err("give exactly one covenant id".into()) };
        let cov = cov_arg(id)?;
        let node = c.need_node()?;
        let indexer = c.need_indexer()?;
        let live = data::live_order(indexer, node, &c.network, &cov)?;
        if live.order.state.maker() != signer.pubkey {
            return Err("this order belongs to another key: only its maker can amend it".into());
        }
        let ms = markets_of(&c, &live)?;
        if super::replan::replans(&live.order.state) {
            return replan_replace(&c, &p, &signer, &live, &ms);
        }
        for k in ["--take-profit", "--exit-tip", "--trail-step", "--trail-gap", "--entry-stop", "--carrier"] {
            if p.get(k).is_some() {
                return Err(format!(
                    "{k} does not apply to a {} (it is amended with --price, --stop, --stop-limit, --amount, --tip, --add-kas, --renew)",
                    live.order.state.template_id().name()
                ));
            }
        }
        let m = &ms[0];
        let u = m.unit();
        let price = |k: &str| p.get(k).map(|v| parse_kas_price(v, &u, live.order.state.scale(), k, false)).transpose();
        let input = AmendInput {
            price: price("--price")?,
            stop: price("--stop")?,
            stop_limit: price("--stop-limit")?,
            amount: p.get("--amount").map(|v| parse_token_amount(v, &u, "--amount")).transpose()?,
            tip: p.get("--tip").map(|v| parse_kas_price(v, &u, live.order.state.scale(), "--tip", true)).transpose()?,
            add_kas: p.get("--add-kas").map(|v| parse_kas(v, "--add-kas", false)).transpose()?.unwrap_or(0),
            renew: p.has("--renew"),
        };
        if input == AmendInput::default() {
            return Err("nothing to change: give --price, --amount, --tip, --stop, --stop-limit, --add-kas or --renew".into());
        }
        let clock = data::clock(node)?;
        let (rep, value, mut notes) = replacement(&live, &input, clock.daa, m.tick)?;
        let funding = data::funding(node, &c.network, &signer.pubkey)?;
        let token_utxos = if rep.custodies().iter().any(|x| x.1 > 0) {
            data::token_utxos(indexer, node, &c.network, m, &signer.pubkey)?
        } else {
            vec![]
        };
        let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
        let (built, in_place, more) = build_replace(&live, &rep, value, &funding, &token_utxos, &ms, &signer.pubkey, &fee, None)?;
        notes.extend(more);
        let mut lines = order_lines(&live, &ms);
        lines.push(format!("replacement: {} holding {}", rep.template_id().name(), units::kas(value as i128)));
        if let Some(a) = rep.amount_left() {
            lines.push(format!("amount: {}", units::tokens(a as i128, &u)));
        }
        let mut issues: Vec<Issue> = notes.into_iter().map(|n| issue("REPLACE_NOTE", Severity::Info, n)).collect();
        issues.extend(ms.iter().filter_map(|m| m.listing_issue(true)));
        Ok((
            Ready {
                title: format!("{} {}", if in_place { "amend in place" } else { "cancel-replace" }, to_hex(&cov)),
                lines,
                issues,
                file: format!("{}.replace.json", to_hex(&cov)),
                json: json!({"order": to_hex(&cov), "inPlace": in_place, "replacement": rep, "value": value.to_string()}),
                built,
            },
            units_of(&ms),
        ))
    })();
    match r {
        Ok((ready, units)) => finish(&c, &signer, ready, &units, "this amendment"),
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// The KAS on every covenant UTXO of an order (its delivery or exit carrier, else its custody's carrier).
fn order_carrier(live: &Live) -> Option<u64> {
    let c = match &live.order.state {
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => Some(x.delivery_carrier),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => Some(x.delivery_carrier),
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => Some(x.exit_carrier),
        AnyState::KobPair(x) => Some(x.delivery_carrier),
        AnyState::KobCondPair(x) => Some(x.delivery_carrier),
        AnyState::KobIfdPair(x) => Some(x.delivery_carrier),
        _ => None,
    };
    c.map(|c| c as u64).or(live.custody.as_ref().map(|u| u.utxo.amount)).filter(|c| *c >= super::place::MIN_CARRIER)
}

/// `kob replace` of a trailing stop, an if-done entry or a pair order: the order as its intent with the changes, re-planned
/// where the order's own KAS and custody come back, then cancelled and replaced in one transaction.
fn replan_replace(c: &Common, p: &Parsed, signer: &Signer, live: &Live, ms: &[TokenMarket]) -> Result<(Ready, Units), String> {
    use super::replan::{intent_from_order, lifetime_of, replace_env, Changes};
    let node = c.need_node()?;
    if p.get("--add-kas").is_some() {
        return Err("--add-kas applies to a plain buy order; this order is re-planned from its terms".into());
    }
    let s = &live.order.state;
    let a = &ms[0];
    let b = ms.get(1);
    let ua = a.unit();
    let scale = s.scale();
    let price = |k: &str| -> Result<Option<i64>, String> {
        p.get(k)
            .map(|v| match b {
                Some(bm) => units::parse_pair_price(v, &ua, scale, &bm.unit(), k),
                None => parse_kas_price(v, &ua, scale, k, false),
            })
            .transpose()
    };
    let delta = |k: &str| -> Result<Option<i64>, String> {
        p.get(k)
            .map(|v| match b {
                Some(bm) => units::parse_pair_delta(v, &ua, scale, &bm.unit(), k),
                None => parse_kas_price(v, &ua, scale, k, true),
            })
            .transpose()
    };
    let tip = |k: &str| p.get(k).map(|v| parse_kas_price(v, &ua, scale, k, true)).transpose();
    let ch = Changes {
        price: price("--price")?,
        take_profit: price("--take-profit")?,
        stop: price("--stop")?,
        stop_limit: price("--stop-limit")?,
        amount: p.get("--amount").map(|v| parse_token_amount(v, &ua, "--amount")).transpose()?,
        tip: tip("--tip")?,
        exit_tip: tip("--exit-tip")?,
        trail_step: delta("--trail-step")?,
        trail_gap: delta("--trail-gap")?,
        entry_stop: price("--entry-stop")?,
        renew: p.has("--renew"),
    };
    if ch == Changes::default() && p.get("--carrier").is_none() {
        return Err("nothing to change: give a new price, stop, amount, tip, trail, --carrier or --renew".into());
    }
    let clock = data::clock(node)?;
    let expiry = s.expiry().map(|e| e.0).unwrap_or(0);
    let lifetime = lifetime_of(expiry, live.genesis_daa, live.deadline, ch.renew);
    let intent = intent_from_order(s, lifetime, clock.daa, &ch)?;
    // what the replacement draws on: the order's own KAS and custody (stand-ins) and the maker's free tokens of A and B
    let needs = super::place::Needs { a: true, b: b.is_some() };
    let (mut env, notes) = super::place::env_of(c, a, b, needs, signer.pubkey)?;
    env.carrier = super::place::carrier_arg(p)?.or(order_carrier(live));
    env.guards_acknowledged = p.has("--ack-no-guards");
    env.market_start_ack = true;
    let penv = replace_env(&env, &live.cov, s, live.order.utxo.amount);
    let plan = super::plan::plan_order(&penv, &intent);
    let Some(req) = plan.request.clone().filter(|_| plan.ok) else {
        super::flow::print_issues(&plan.issues, c.json);
        return Err("the replacement could not be planned: nothing signed".into());
    };
    let rep = req.order.clone();
    let value = req.value as i64;
    let token_carrier = (req.token_carrier > 0).then_some(req.token_carrier);
    let mut token_utxos = env.token_utxos.clone();
    if let Some(pp) = &env.pair {
        token_utxos.extend(pp.quote_utxos.iter().cloned());
    }
    let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
    let (built, _, more) = build_replace(live, &rep, value, &env.funding, &token_utxos, ms, &signer.pubkey, &fee, token_carrier)?;
    let mut lines = order_lines(live, ms);
    lines.push(format!("replacement: {} holding {}", rep.template_id().name(), units::kas(value as i128)));
    lines.extend(super::place::plan_lines(&plan, a, b).into_iter().map(|l| format!("  {l}")));
    let mut issues: Vec<Issue> = plan.issues.iter().filter(|i| i.severity != Severity::Error).cloned().collect();
    issues.extend(notes);
    // a registry token that is not tradable (delisted, its program pending review) is refused here as for every amendment
    issues.extend(ms.iter().filter_map(|m| m.listing_issue(true)));
    issues.extend(more.into_iter().map(|n| issue("REPLACE_NOTE", Severity::Info, n)));
    let mut states = vec![serde_json::to_value(&rep).unwrap_or_default()];
    states.extend(plan.states.iter().skip(1).map(|x| serde_json::to_value(x).unwrap_or_default()));
    Ok((
        Ready {
            title: format!("cancel-replace {}", to_hex(&live.cov)),
            lines,
            issues,
            file: format!("{}.replace.json", to_hex(&live.cov)),
            json: json!({"order": to_hex(&live.cov), "inPlace": false, "replacement": rep, "states": states, "value": value.to_string()}),
            built,
        },
        units_of(ms),
    ))
}

// ------------------------------------------------------------------------------------------------ cancel

/// Help of `kob cancel`.
pub const CANCEL_USAGE: &str = "\
kob cancel: the maker's cancel of live orders by covenant id (the indexer's view, every UTXO checked on the node)

USAGE
  kob cancel <COVENANT_ID>... [options]

  The order's KAS, its custody, the strays of its own token(s) and foreign strays (other tokens sent to it) return to
  the maker in one transaction per order; strays one transaction cannot take are reported. Fee from the released KAS,
  else the smallest sufficient KAS UTXO of the key. (`kob order cancel --view FILE` does the same from saved views.)
";

/// Run `kob cancel`.
pub fn run_cancel(args: &[String]) -> i32 {
    let (p, c, signer) = match ctx(args, &[], &[], CANCEL_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    if p.pos.is_empty() {
        eprintln!("error: give at least one covenant id");
        return 2;
    }
    let mut code = 0;
    for id in &p.pos {
        let r = (|| -> Result<(Ready, Units), String> {
            let cov = cov_arg(id)?;
            let node = c.need_node()?.to_string();
            let indexer = c.need_indexer()?;
            let v = super::http::get_ok(indexer, &format!("/v1/orders/{}", to_hex(&cov)), data::INDEXER_TIMEOUT)?;
            let parsed = crate::inputs::parse(&v, &format!("indexer order {}", to_hex(&cov)), &c.network)?;
            let view = parsed.views.into_iter().next().ok_or("the indexer's view of the order is not usable")?;
            let a = crate::order::Args {
                op: crate::order::Op::Cancel,
                views: vec![],
                key: String::new(),
                node: node.clone(),
                network: c.network.clone(),
                dry_run: true,
                fee_rate: c.fee_rate.unwrap_or(kob_protocol::tx::MIN_FEE_RATE),
                out_dir: None,
                out: None,
            };
            let prepared = match crate::order::prepare(&view, &a, signer.pubkey)? {
                Ok(x) => x,
                Err(o) => return Err(o.json["reason"].as_str().unwrap_or("refused").to_string()),
            };
            let live = data::live_order(indexer, &node, &c.network, &cov).ok();
            let ms = live.as_ref().and_then(|l| markets_of(&c, l).ok()).unwrap_or_default();
            let mut lines = live.as_ref().map(|l| order_lines(l, &ms)).unwrap_or_default();
            lines.push(prepared.disclosure.clone());
            let issues = prepared
                .left
                .iter()
                .map(|l| {
                    issue(
                        "CANCEL_LEFT_BEHIND",
                        Severity::Warning,
                        format!("left behind {}: {}", l["outpoint"].as_str().unwrap_or(""), l["reason"].as_str().unwrap_or("")),
                    )
                })
                .collect();
            Ok((
                Ready {
                    title: format!("cancel {}", to_hex(&cov)),
                    lines,
                    issues,
                    file: prepared.name,
                    json: prepared.info,
                    built: prepared.built,
                },
                units_of(&ms),
            ))
        })();
        let c2 = finish_or(&c, &signer, r, "this cancel");
        if c2 != 0 {
            code = c2;
        }
    }
    code
}

fn finish_or(c: &Common, signer: &Signer, r: Result<(Ready, Units), String>, what: &str) -> i32 {
    match r {
        Ok((ready, units)) => finish(c, signer, ready, &units, what),
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ------------------------------------------------------------------------------------------------ refund

/// Help of `kob refund`.
pub const REFUND_USAGE: &str = "\
kob refund: the permissionless refund of an order that has ended (expiry, 90 days idle, an IOC / FOK's kill time)

USAGE
  kob refund <COVENANT_ID> [--no-funding] [options]

  The order's KAS and custody go back to its maker; whoever sends the refund (any key: a keeper) keeps the order's
  refundTip minus the network fee, as change to the signing key. Like the keepers, a refund first adds one KAS UTXO of
  the key (at least 1 KAS) so the tip comes back as change, and goes without it when that earns more (--no-funding: never).
  A refund never moves strays: the maker's cancel does.
";

/// Run `kob refund`.
pub fn run_refund(args: &[String]) -> i32 {
    let (p, c, signer) = match ctx(args, &[], &["--no-funding"], REFUND_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let r = (|| -> Result<(Ready, Units), String> {
        let [id] = p.pos.as_slice() else { return Err("give exactly one covenant id".into()) };
        let cov = cov_arg(id)?;
        let node = c.need_node()?;
        let indexer = c.need_indexer()?;
        let live = data::live_order(indexer, node, &c.network, &cov)?;
        let clock = data::clock(node)?;
        let due = live.order.state.refund_due(live.order.utxo.block_daa_score as i64).ok_or("no refund time")?;
        if due > clock.daa {
            return Err(format!("not refundable before DAA {due} (now {}, about {} s)", clock.daa, (due - clock.daa) / 10));
        }
        let ms = markets_of(&c, &live).unwrap_or_default();
        let funding = data::funding(node, &c.network, &signer.pubkey)?;
        let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
        let holding = live.order.state.custody_amount().unwrap_or(0) > 0 || live.order.state.is_pair();
        let make = |f: Vec<KeyUtxo>| RefundOrder {
            order: live.order.clone(),
            custody: if holding { live.custody.clone() } else { None },
            prefund: live.prefund.clone(),
            foreign: vec![],
            lock_time: due as u64,
            funding: f,
            change: Some(signer.pubkey),
            fee: fee.clone(),
        };
        let earned = |b: &BuiltTx| -> i128 {
            let me = kob_protocol::tx::spk_to_string(&p2pk_spk(&signer.pubkey));
            let out: i128 =
                b.tx.outputs.iter().filter(|o| o.script_public_key == me && o.covenant.is_none()).map(|o| o.value as i128).sum();
            let inp: i128 =
                b.tx.inputs
                    .iter()
                    .zip(&b.plans)
                    .filter(|(_, pl)| matches!(pl, kob_protocol::tx::SigPlan::P2pk { pubkey } if *pubkey == signer.pubkey))
                    .map(|(i, _)| i.utxo.amount as i128)
                    .sum();
            out - inp
        };
        let seed: Option<KeyUtxo> = if p.has("--no-funding") {
            None
        } else {
            let mut f: Vec<&KeyUtxo> = funding.iter().filter(|u| u.utxo.amount >= ONE_KAS).collect();
            f.sort_by_key(|u| u.utxo.amount);
            f.first().map(|u| (*u).clone())
        };
        let with = seed.and_then(|s| build(&Action::RefundOrder(make(vec![s]))).ok());
        let without = build(&Action::RefundOrder(make(vec![])));
        let built = match (with, without) {
            (Some(a), Ok(b)) => {
                if earned(&a) >= earned(&b) {
                    a
                } else {
                    b
                }
            }
            (Some(a), Err(_)) => a,
            (None, Ok(b)) => b,
            (None, Err(e)) => return Err(format!("the protocol refused the refund: {e}")),
        };
        let gain = earned(&built);
        let mut lines = order_lines(&live, &ms);
        lines.push(format!("refundable since DAA {due} (now {})", clock.daa));
        lines.push(format!("the order's KAS and custody return to its maker {}", show_id(&live.order.state.maker(), "")));
        lines.push(format!(
            "refund tip: {}; you {} {} after the fee",
            units::kas(refund_tip(&live.order.state) as i128),
            if gain >= 0 { "earn" } else { "pay" },
            units::kas(gain.abs())
        ));
        let mut issues = vec![];
        if gain < 0 {
            issues.push(issue(
                "REFUND_COSTS",
                Severity::Warning,
                "the refund tip does not cover the network fee: this refund costs you KAS",
            ));
        }
        if !live.strays.is_empty() || live.foreign > 0 {
            issues.push(issue(
                "REFUND_STRAYS_STAY",
                Severity::Warning,
                "a refund does not move stray tokens sent to this order; the maker's cancel does",
            ));
        }
        Ok((
            Ready {
                title: format!("refund {}", to_hex(&cov)),
                lines,
                issues,
                file: format!("{}.refund.json", to_hex(&cov)),
                json: json!({"order": to_hex(&cov), "due": due, "earned": gain.to_string()}),
                built,
            },
            units_of(&ms),
        ))
    })();
    finish_or(&c, &signer, r, "this refund")
}

// ------------------------------------------------------------------------------------------------ take

/// Help of `kob take`.
pub const TAKE_USAGE: &str = "\
kob take: fill a resting order as the taker, at its own price (the matchers' transaction with one order)

USAGE
  kob take <COVENANT_ID> --amount AMOUNT [--carrier KAS] [options]

  An order that sells (a limit / IOC / FOK / Dutch sell, a take-profit sell, a sell-first entry): you pay its price plus
  tip in KAS from your KAS UTXOs and receive AMOUNT of the token (on a token output carrying --carrier, default 2KAS).
  An order that buys (a limit buy, a take-profit buy, a buy-first entry): you deliver AMOUNT of your tokens and receive
  its price plus tip in KAS. The fill must respect the order's minimum fill, its slices and its activation; a decaying
  order is filled at its price at the transaction's lock time (now - 10 DAA). Pair orders and stops are not taken here.
";

/// Run `kob take`.
pub fn run_take(args: &[String]) -> i32 {
    let (p, c, signer) = match ctx(args, &["--amount", "--carrier"], &[], TAKE_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let r = (|| -> Result<(Ready, Units), String> {
        let [id] = p.pos.as_slice() else { return Err("give exactly one covenant id".into()) };
        let cov = cov_arg(id)?;
        let node = c.need_node()?;
        let indexer = c.need_indexer()?;
        let live = data::live_order(indexer, node, &c.network, &cov)?;
        if live.order.state.maker() == signer.pubkey {
            return Err("this is your own order: a self-trade only pays fees (cancel or amend it instead)".into());
        }
        let ms = markets_of(&c, &live)?;
        let m = &ms[0];
        let amount = parse_token_amount(p.get("--amount").ok_or("--amount is required")?, &m.unit(), "--amount")?;
        let carrier = match p.get("--carrier") {
            Some(v) => parse_kas(v, "--carrier", false)?,
            None => DEFAULT_ORDER_CARRIER,
        };
        if carrier < super::place::MIN_CARRIER {
            return Err("--carrier must be at least 0.08KAS".into());
        }
        let clock = data::clock(node)?;
        let lock = (clock.daa - LOCK_MARGIN_DAA).max(0);
        let u = live.order.utxo.clone();
        let custody = || live.custody.clone().ok_or("the order's custody is unknown");
        let (leg, sells) = match &live.order.state {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
                (Leg::Ask { order: OrderUtxo { utxo: u, state: s.clone() }, custody: custody()?, amount, t: None }, true)
            }
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
                (Leg::Bid { order: OrderUtxo { utxo: u, state: s.clone() }, amount, t: None }, false)
            }
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) if s.tp_price > 0 => (
                Leg::CondAsk {
                    order: OrderUtxo { utxo: u, state: s.clone() },
                    custody: custody()?,
                    amount,
                    leg: 0,
                    evidence: None,
                    t: None,
                    merge: None,
                },
                true,
            ),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) if s.tp_price > 0 => (
                Leg::CondBid { order: OrderUtxo { utxo: u, state: s.clone() }, amount, leg: 0, evidence: None, t: None, merge: None },
                false,
            ),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) if s.entry_stop == 0 => {
                (Leg::IfdBid { order: OrderUtxo { utxo: u, state: s.clone() }, amount, evidence: None, t: None }, false)
            }
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) if s.entry_stop == 0 => (
                Leg::IfdAsk { order: OrderUtxo { utxo: u, state: s.clone() }, custody: custody()?, amount, evidence: None, t: None },
                true,
            ),
            other => {
                return Err(format!(
                    "a {} is not taken here (pair orders and stop legs fill through the matchers)",
                    other.template_id().name()
                ))
            }
        };
        let funding = data::funding(node, &c.network, &signer.pubkey)?;
        let taker_tokens = if sells {
            vec![]
        } else {
            let mine = data::token_utxos(indexer, node, &c.network, m, &signer.pubkey)?;
            let usable = usable_token_utxos(m, &signer.pubkey, &mine);
            select_tokens(&usable, amount, m.slots.0.min(MAX_TOKEN_INPUTS), &m.name()).map_err(|e| e.message)?
        };
        let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
        let make = |f: Vec<KeyUtxo>| Batch {
            lock_time: lock as u64,
            legs: vec![leg.clone()],
            updates: vec![],
            taker_tokens: taker_tokens.clone(),
            taker: Some(signer.pubkey),
            taker_token_carrier: carrier,
            keep_surplus: vec![],
            keep_carrier: None,
            receivers: vec![],
            payments: vec![],
            funding: f,
            change: Some(signer.pubkey),
            records: vec![],
            fee: fee.clone(),
        };
        // the fewest of the key's coins, largest first, that pay the fee and the carriers (x402 swap's rule)
        let mut coins = funding.clone();
        coins.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
        let mut built = None;
        let mut last = String::new();
        for n in 0..=coins.len().min(super::plan::MAX_FUNDING_INPUTS) {
            match build(&Action::Batch(make(coins[..n].to_vec()))) {
                Ok(b) => {
                    built = Some(b);
                    break;
                }
                Err(e @ kob_protocol::Error::InsufficientFunds { .. }) => last = e.to_string(),
                Err(e) if n == 0 && !coins.is_empty() => last = e.to_string(),
                Err(e) => return Err(format!("the protocol refused the fill: {e}")),
            }
        }
        let built = built.ok_or_else(|| format!("cannot fund the fill: {last}"))?;
        let mut lines = order_lines(&live, &ms);
        lines.push(format!(
            "you {} {} at the order's price at DAA {lock}",
            if sells { "buy" } else { "sell" },
            units::tokens(amount as i128, &m.unit())
        ));
        if let Some(pr) = taker_price(&live.order.state, lock, live.order.utxo.block_daa_score as i64) {
            lines.push(format!("order price now: {}", units::price_per_whole(pr, &m.unit(), live.order.state.scale(), 8, "KAS")));
        }
        Ok((
            Ready {
                title: format!("take {} {}", to_hex(&cov), if sells { "(buy from a sell order)" } else { "(sell into a buy order)" }),
                lines,
                issues: ms.iter().filter_map(|m| m.listing_issue(true)).collect(),
                file: format!("{}.take.json", to_hex(&cov)),
                json: json!({"order": to_hex(&cov), "amount": amount.to_string(), "lockTime": lock}),
                built,
            },
            units_of(&ms),
        ))
    })();
    finish_or(&c, &signer, r, "this fill")
}

/// The refund tip of any order kind.
pub fn refund_tip(s: &AnyState) -> i64 {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.refund_tip,
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.refund_tip,
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.refund_tip,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.refund_tip,
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => x.refund_tip,
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => x.refund_tip,
        AnyState::KobPair(x) => x.refund_tip,
        AnyState::KobCondPair(x) => x.refund_tip,
        AnyState::KobIfdPair(x) => x.refund_tip,
    }
}

fn taker_price(s: &AnyState, t: i64, utxo_daa: i64) -> Option<i64> {
    match s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.price_at(t, utxo_daa),
        AnyState::KobBid(x) | AnyState::KobBidKron(x) => x.price_at(t, utxo_daa),
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => Some(x.tp_price),
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => Some(x.tp_price),
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => Some(x.price),
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => Some(x.price),
        _ => None,
    }
}

// ------------------------------------------------------------------------------------------------ send

/// Help of `kob send`.
pub const SEND_USAGE: &str = "\
kob send: send KAS or a token

USAGE
  kob send KAS <TO> <AMOUNT> [options]          AMOUNT e.g. 2.5KAS; TO an address of the network or a 64-hex x-only key
  kob send <TOKEN> <TO> <AMOUNT> [--carrier KAS] [options]
                                                TOKEN a ticker or covenant id; AMOUNT e.g. 12.5TUSD or 1250base; TO a P2PK
                                                address or a 64-hex x-only key; the recipient's token output carries
                                                --carrier (default 2KAS, at least 0.08KAS and the program's floor)

  A token transfer stays within its program's slots (the standard 3/3 program: at most 3 token inputs and 3 token
  outputs: the recipient and your change), taking the fewest of your token UTXOs; KAS for the carrier and the fee
  come from your KAS UTXOs, largest first.
";

/// A plain KAS payment from P2PK coins of `me` (a version-1 transaction, the builders' P2PK inputs): the payment and
/// the change back to `me` (no change when it would be dust); the fee is the relay floor at the requested rate.
pub fn kas_payment(
    coins: &[KeyUtxo],
    amount: u64,
    to: &kaspa_consensus_core::tx::ScriptPublicKey,
    me: &[u8; 32],
    fee: &FeeOptions,
) -> Result<BuiltTx, String> {
    use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
    use kaspa_consensus_core::tx::{Transaction, TransactionInput, TransactionOutput, UtxoEntry};
    use kob_protocol::tx::{masses, sighash, target_fee, FeeReport, SigPlan, SignRequest, TxJson, DUST_OUTPUT_MIN, MIN_FEE_RATE};
    let rate = fee.fee_rate.unwrap_or(MIN_FEE_RATE);
    let budget = kob_protocol::budget::lookup("p2pk").map_err(|e| e.to_string())?;
    let total: u64 = coins.iter().map(|k| k.utxo.amount).sum();
    let mine = p2pk_spk(me);
    let entries: Vec<UtxoEntry> =
        coins.iter().map(|k| UtxoEntry::new(k.utxo.amount, p2pk_spk(&k.pubkey), k.utxo.block_daa_score, false, None)).collect();
    let placeholder = {
        let mut v = vec![0x41u8];
        v.extend([0u8; 65]);
        v
    };
    let tx_of = |outs: Vec<TransactionOutput>, sig: &[u8]| {
        let ins = coins
            .iter()
            .map(|k| {
                TransactionInput::new_with_compute_budget(k.utxo.outpoint(), sig.to_vec(), kob_protocol::tx::SEQUENCE_NONFINAL, budget)
            })
            .collect();
        let tx = Transaction::new(1, ins, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
        let m = masses(&tx, &entries);
        tx.set_storage_mass(m.storage);
        tx
    };
    let pay = TransactionOutput::new(amount, to.clone());
    // with change: the fee is a fixed point (the change's value does not change the relay floor)
    let mut chosen: Option<(Vec<TransactionOutput>, u64, Option<u32>)> = None;
    let mut f = 0u64;
    // whether what is left over is too small to be its own output (only then may it go to the fee)
    let mut change_is_dust = false;
    for _ in 0..16 {
        let Some(change) = total.checked_sub(amount).and_then(|x| x.checked_sub(f)) else {
            change_is_dust = true;
            break;
        };
        if change < DUST_OUTPUT_MIN {
            change_is_dust = true;
            break;
        }
        let outs = vec![pay.clone(), TransactionOutput::new(change, mine.clone())];
        let t = tx_of(outs.clone(), &placeholder);
        let need = target_fee(&masses(&t, &entries), rate, fee.fee_mode);
        if need <= f {
            if masses(&t, &entries).within_block_limits() {
                chosen = Some((outs, f, Some(1)));
            }
            break;
        }
        f = need;
    }
    if chosen.is_none() {
        let outs = vec![pay.clone()];
        let t = tx_of(outs.clone(), &placeholder);
        let need = target_fee(&masses(&t, &entries), rate, fee.fee_mode);
        if total < amount.saturating_add(need) {
            return Err(format!("insufficient funds: need {} sompi, have {total} sompi", amount.saturating_add(need)));
        }
        // a change that is not dust but did not fit (the transaction with it exceeds a block mass limit, e.g. the KIP-9
        // storage mass of a payment just above the dust bound plus the change) is never paid to the network as fee
        let surplus = total - amount - need;
        if !change_is_dust && surplus >= DUST_OUTPUT_MIN {
            return Err(format!(
                "the change of {surplus} sompi does not fit as its own output (the transaction with it exceeds a block mass \
                 limit) and would otherwise be paid as network fee: send a larger amount"
            ));
        }
        chosen = Some((outs, total - amount, None));
    }
    let (outs, paid, change_output) = chosen.expect("set above");
    let sized = tx_of(outs.clone(), &placeholder);
    let report = masses(&sized, &entries);
    let tx = tx_of(outs, &[]);
    let sign = (0..coins.len())
        .map(|i| SignRequest {
            input_index: i,
            pubkey: coins[i].pubkey,
            sighash_type: 1,
            sighash: sighash(&tx, &entries, i),
            redeem_script: None,
        })
        .collect();
    Ok(BuiltTx {
        tx: TxJson::from_tx(&tx, &entries),
        plans: coins.iter().map(|k| SigPlan::P2pk { pubkey: k.pubkey }).collect(),
        roles: coins.iter().map(|_| "p2pk".to_string()).collect(),
        sign,
        fee: FeeReport {
            fee: paid,
            min_fee: target_fee(&report, rate, fee.fee_mode),
            fee_rate: rate,
            fee_mode: fee.fee_mode,
            mass: report,
            change_output,
        },
        covenants: vec![],
    })
}

/// A recipient: its script and, for a P2PK one, its x-only key.
pub fn recipient(to: &str, network: &str) -> Result<(kaspa_consensus_core::tx::ScriptPublicKey, Option<[u8; 32]>), String> {
    if let Some(k) = kob_protocol::registry::parse_hex32(to) {
        kob_protocol::tx::check_key(&k, "recipient").map_err(|e| e.to_string())?;
        return Ok((p2pk_spk(&k), Some(k)));
    }
    let a = kaspa_addresses::Address::try_from(to).map_err(|e| format!("`{to}` is not an address or a 64-hex key: {e}"))?;
    if a.prefix != crate::common::prefix_for_network(network)? {
        return Err(format!("`{to}` is an address of another network than {network}"));
    }
    let spk = kaspa_txscript::pay_to_address_script(&a);
    let key = match a.version {
        kaspa_addresses::Version::PubKey => <[u8; 32]>::try_from(a.payload.as_slice()).ok(),
        _ => None,
    };
    Ok((spk, key))
}

/// Run `kob send`.
pub fn run_send(args: &[String]) -> i32 {
    let (p, c, signer) = match ctx(args, &["--carrier"], &[], SEND_USAGE) {
        Ok(x) => x,
        Err(code) => return code,
    };
    let r = (|| -> Result<(Ready, Units), String> {
        let [what, to, amount] = p.pos.as_slice() else { return Err("usage: kob send <TOKEN|KAS> <TO> <AMOUNT>".into()) };
        let node = c.need_node()?;
        let (spk, key) = recipient(to, &c.network)?;
        let funding = data::funding(node, &c.network, &signer.pubkey)?;
        let mut coins = funding.clone();
        coins.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
        let fee = FeeOptions { fee_rate: c.fee_rate, ..Default::default() };
        if what.eq_ignore_ascii_case("kas") {
            if p.get("--carrier").is_some() {
                return Err("--carrier applies to a token send".into());
            }
            let sompi = parse_kas(amount, "the amount", false)?;
            let mut built = None;
            let mut last = String::new();
            for n in 1..=coins.len().min(super::plan::MAX_FUNDING_INPUTS) {
                match kas_payment(&coins[..n], sompi, &spk, &signer.pubkey, &fee) {
                    Ok(b) => {
                        built = Some(b);
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            let built = built.ok_or_else(|| format!("not enough KAS: {last}"))?;
            return Ok((
                Ready {
                    title: format!("send {} to {to}", units::kas(sompi as i128)),
                    lines: vec![format!("recipient: {to}"), format!("amount: {}", units::kas(sompi as i128))],
                    issues: vec![],
                    file: format!("send-{}.json", to_hex(&built.tx.id)),
                    json: json!({"to": to, "amount": sompi.to_string()}),
                    built,
                },
                Units::new(),
            ));
        }
        let indexer = c.need_indexer()?;
        let (reg, idx) = data::token_lists(c.registry.as_deref(), &c.network, Some(indexer))?;
        let order_scale = |cov: &[u8; 32]| data::order_scale(indexer, cov);
        let remembered = |cov: &[u8; 32]| crate::recent::first_order_decimals(&c.network, cov);
        let declaration = |cov: &[u8; 32]| data::declaration(indexer, cov);
        let held_program = |cov: &[u8; 32]| data::held_program(indexer, node, &c.network, cov, &signer.pubkey);
        let open = market::OpenToken {
            remembered: Some(&remembered),
            declaration: Some(&declaration),
            held_program: Some(&held_program),
            ..market::OpenToken::new(&c.network, &order_scale)
        };
        let m = market::resolve_with(what, &reg, &idx, Some(&open))?;
        let key = key.ok_or("a token goes to a P2PK address or an x-only key (the token state names its owner key)")?;
        let base = parse_token_amount(amount, &m.unit(), "the amount")?;
        let carrier = match p.get("--carrier") {
            Some(v) => parse_kas(v, "--carrier", false)?,
            None => DEFAULT_ORDER_CARRIER,
        };
        let floor = super::plan::token_carrier_floor(&m).max(super::place::MIN_CARRIER as i64) as u64;
        if carrier < floor {
            return Err(format!("--carrier must be at least {} for a token output of {}", units::kas(floor as i128), m.name()));
        }
        let mine = data::token_utxos(indexer, node, &c.network, &m, &signer.pubkey)?;
        let usable = usable_token_utxos(&m, &signer.pubkey, &mine);
        let tokens = select_tokens(&usable, base, m.slots.0.min(MAX_TOKEN_INPUTS), &m.name()).map_err(|e| e.message)?;
        let change: i64 = tokens.iter().map(|t| t.state.amount()).sum::<i64>() - base;
        let outputs = 1 + usize::from(change > 0);
        if outputs > m.slots.1 {
            return Err(format!("{} allows {} token outputs per transfer", m.program.name(), m.slots.1));
        }
        let make = |f: Vec<KeyUtxo>| SendTokens {
            token: TokenRef { covenant_id: m.cov, program: m.program },
            tokens: tokens.clone(),
            recipients: vec![TokenRecipient { pubkey: key, amount: base, carrier }],
            token_change: None,
            token_change_carrier: carrier,
            funding: f,
            change: Some(signer.pubkey),
            records: vec![],
            fee: fee.clone(),
        };
        let mut built = None;
        let mut last = String::new();
        for n in 0..=coins.len().min(super::plan::MAX_FUNDING_INPUTS) {
            match build(&Action::SendTokens(make(coins[..n].to_vec()))) {
                Ok(b) => {
                    built = Some(b);
                    break;
                }
                Err(e @ kob_protocol::Error::InsufficientFunds { .. }) => last = e.to_string(),
                Err(e) if n == 0 && !coins.is_empty() => last = e.to_string(),
                Err(e) => return Err(format!("the protocol refused the transfer: {e}")),
            }
        }
        let built = built.ok_or_else(|| format!("cannot fund the transfer: {last}"))?;
        let mut units = Units::new();
        units.insert(m.cov, m.unit());
        Ok((
            Ready {
                title: format!("send {} to {to}", units::tokens(base as i128, &m.unit())),
                lines: vec![
                    format!("token: {} ({})", m.name(), m.program.name()),
                    format!("recipient: {to}"),
                    format!("amount: {}", units::tokens(base as i128, &m.unit())),
                    format!("recipient's token output carries {}", units::kas(carrier as i128)),
                ],
                issues: m.listing_issue(false).into_iter().collect(),
                file: format!("send-{}.json", to_hex(&built.tx.id)),
                json: json!({"to": to, "token": to_hex(&m.cov), "amount": base.to_string(), "carrier": carrier.to_string()}),
                built,
            },
            units,
        ))
    })();
    finish_or(&c, &signer, r, "this transfer")
}
