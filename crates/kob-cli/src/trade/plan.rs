//! The order planner of the trading commands: a port of the web app's planning layer (`web/src/kob/plan.ts`,
//! `orders/simple.ts`, `orders/cond*.ts`, `orders/pair-simple.ts`, `orders/common.ts`, `orders/pair-common.ts`, `guards.ts`,
//! `daa.ts`, `funding.ts`) for the order kinds `kob place` offers, so the CLI places the same states, the same `createOrder`
//! requests and therefore the same transactions and `KOB1` placement records as the wallet for the same inputs:
//!
//! * the wallet defaults (`docs/spec/matcher.md` §10): minimum fill worth 10 KAS at the limit (1 for IOC / FOK / market,
//!   `ceil(amount / 4)` for if-done entries), 2 KAS on every covenant UTXO, tip 0, GTC = 90 days, IOC / FOK life 300 DAA,
//!   market auctions from the touch over 200 DAA to a 3 % bound starting 30 DAA after placement, stop bands 3 % over
//!   300 DAA, trigger threshold max(minimum fill, 25 % of the amount), rest time 50 DAA, the program's refund / keeper tips;
//! * the guards: tick, amount and value bounds, 90-day horizon, self-trade, FOK depth, crossing limits (auction / limit /
//!   reject), price bands, market-start checks against the last fill;
//! * the coin selection (largest KAS first with a fee reserve that grows by the reported shortfall; the smallest single
//!   token UTXO that covers the amount, else the largest first within the program's inputs) and the recover-and-compare
//!   self-check of the placement record.
//!
//! Every number is the covenant's own arithmetic (`kob_protocol::state`); prices are the state's (quote units per `scale`
//! base units), amounts base units, KAS sompi.

use kob_protocol::build::{build, check_new_order, min_order_value, Action, CreateOrder};
use kob_protocol::defaults::{
    day_order, default_min_fill, default_min_fill_ifd, default_min_fill_pair, default_min_touch, pair_tips, DEFAULT_ORDER_CARRIER,
};
use kob_protocol::payload::recover_orders;
use kob_protocol::state::{
    AnyState, AskState, BidState, CondAskState, CondBidState, IfdAskState, IfdBidState, PairState, SIDE_ASK, SIDE_BID,
};
use kob_protocol::tx::{BuiltTx, FeeOptions, KeyUtxo, TokenUtxo, COVENANT_OUTPUT_MIN, DUST_OUTPUT_MIN};
use kob_protocol::Family;

use super::market::TokenMarket;

// ------------------------------------------------------------------------------------------------ constants (daa.ts)

/// 90 days at 10 DAA/s: the covenants' `MAX_IDLE`.
pub const MAX_IDLE_DAA: i64 = 77_760_000;
/// The covenant kill of an IOC / FOK (DAA after it became fillable).
pub const IOC_KILL_DAA: i64 = 600;
/// Wallet life of an IOC / FOK.
pub const IOC_LIFE_DAA: i64 = 300;
/// Market auction length, activation delay and slippage bound.
pub const MARKET_AUCTION_DAA: i64 = 200;
/// Market activation delay.
pub const MARKET_ACTIVATION_DAA: i64 = 30;
/// Market / stop slippage bound (bps).
pub const SLIPPAGE_BPS: i64 = 300;
/// Stop auction length.
pub const STOP_BAND_DAA: i64 = 300;
/// Trigger rest time.
pub const MIN_REST_DAA: i64 = 50;
/// Default network-fee reserve of a placement (`funding.ts`).
pub const DEFAULT_FEE_RESERVE: i128 = 5_000_000;
/// Most KAS inputs of one placement.
pub const MAX_FUNDING_INPUTS: usize = 50;
/// Most token inputs of any transaction.
pub const MAX_TOKEN_INPUTS: usize = 8;
/// Fills a resting bid budgets a delivery carrier for by default.
pub const DEFAULT_BID_FILLS: i64 = 3;
/// Most delivery carriers a resting pair order budgets by default.
pub const MAX_DEFAULT_PAIR_FILLS: i64 = 64;
/// 2^62: every quote of an order stays below it.
pub const QUOTE_LIMIT: i128 = 1 << 62;
/// A market order worth this much (100 KAS) with no independent reference for its start is held until acknowledged.
pub const MARKET_START_UNVERIFIED_MIN_SOMPI: i128 = 100 * 100_000_000;
/// Tolerance of a market start against the last fill (bps).
pub const MARKET_START_TOLERANCE_BPS: i128 = 1_000;
/// KRON outputs hold at most this many base units.
pub const KRON_MAX_OUTPUT_AMOUNT: i64 = kob_protocol::registry::KRON_MAX_OUTPUT_AMOUNT;

// ------------------------------------------------------------------------------------------------ vocabulary

/// Side of the placed order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Sell the token.
    Sell,
    /// Buy the token.
    Buy,
}

impl Side {
    /// `sell` / `buy`.
    pub fn name(self) -> &'static str {
        match self {
            Side::Sell => "sell",
            Side::Buy => "buy",
        }
    }
    fn other(self) -> Side {
        match self {
            Side::Sell => Side::Buy,
            Side::Buy => Side::Sell,
        }
    }
}

/// Severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocks the plan.
    Error,
    /// Shown, does not block.
    Warning,
    /// Informational.
    Info,
}

/// A finding (the web planner's `PlanIssue`: same codes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Stable code (`web/src/kob/orders/*-issues.ts`).
    pub code: &'static str,
    /// Severity.
    pub severity: Severity,
    /// English text.
    pub message: String,
}

fn issue(code: &'static str, severity: Severity, message: impl Into<String>) -> Issue {
    Issue { code, severity, message: message.into() }
}
fn error(code: &'static str, m: impl Into<String>) -> Issue {
    issue(code, Severity::Error, m)
}
fn warning(code: &'static str, m: impl Into<String>) -> Issue {
    issue(code, Severity::Warning, m)
}
fn info(code: &'static str, m: impl Into<String>) -> Issue {
    issue(code, Severity::Info, m)
}
fn has_error(v: &[Issue]) -> bool {
    v.iter().any(|i| i.severity == Severity::Error)
}

/// The node's virtual DAA score and the UTC clock, read together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// DAA score.
    pub daa: i64,
    /// UTC unix seconds.
    pub unix: i64,
    /// DAA rate, milli-DAA per second (10 000 nominal; clamped to 9 500..10 500).
    pub rate_milli: i64,
}

/// One price level of the visible book (price as posted, per whole token; base units).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    /// Price.
    pub price: i64,
    /// Base units.
    pub amount: i64,
    /// Orders at the level.
    pub orders: i64,
    /// Tip.
    pub tip: i64,
}

/// One resting order of the non-aggregated book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookOrder {
    /// Price as posted.
    pub price: i64,
    /// Base units left.
    pub amount: i64,
    /// Tip.
    pub tip: i64,
    /// Minimum fill.
    pub min_fill: Option<i64>,
}

/// The visible book: asks ascending, bids descending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    /// Asks.
    pub asks: Vec<Level>,
    /// Bids.
    pub bids: Vec<Level>,
    /// Per-order asks (exact FOK pre-check).
    pub ask_orders: Option<Vec<BookOrder>>,
    /// Per-order bids.
    pub bid_orders: Option<Vec<BookOrder>>,
}

/// An own resting order (self-trade prevention).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnOrder {
    /// Covenant id.
    pub cov: [u8; 32],
    /// Side.
    pub side: Side,
    /// Price it can trade at.
    pub price: i64,
    /// Tip.
    pub tip: i64,
    /// Base units left.
    pub amount_left: i64,
    /// Can trade now.
    pub active: bool,
}

/// The quote token of a pair A/B and what the planner needs of it.
#[derive(Debug, Clone)]
pub struct PairPart {
    /// B.
    pub quote: TokenMarket,
    /// The maker's B UTXOs.
    pub quote_utxos: Vec<TokenUtxo>,
    /// KAS per whole A (the default minimum fill), when known.
    pub kas_per_whole_a: Option<i64>,
}

/// Everything a planner needs from the outside world (`PlanEnv`).
#[derive(Debug, Clone)]
pub struct Env {
    /// The token (a pair: A).
    pub market: TokenMarket,
    /// The maker (the signing key).
    pub maker: [u8; 32],
    /// Clock.
    pub clock: Clock,
    /// Book (a pair: in B per whole A, no tips).
    pub book: Book,
    /// Own live orders.
    pub own: Vec<OwnOrder>,
    /// Guards whose input could not be read (`own-orders`, `book`).
    pub guards_unavailable: Vec<String>,
    /// The user acknowledged running without them.
    pub guards_acknowledged: bool,
    /// Price of the newest fill (per whole token).
    pub last_fill: Option<i64>,
    /// The user acknowledged the market-start findings.
    pub market_start_ack: bool,
    /// The maker's plain KAS UTXOs.
    pub funding: Vec<KeyUtxo>,
    /// The maker's token UTXOs (a pair: of A).
    pub token_utxos: Vec<TokenUtxo>,
    /// Explicit fee rate (sompi per gram); `None`: the relay floor.
    pub fee_rate: Option<u64>,
    /// KAS on every covenant UTXO (default 2 KAS).
    pub carrier: Option<u64>,
    /// A pair order's quote token B.
    pub pair: Option<PairPart>,
}

impl Env {
    fn carrier(&self) -> i64 {
        self.carrier.unwrap_or(DEFAULT_ORDER_CARRIER) as i64
    }
}

/// When a resting order ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifetime {
    /// 90 days idle.
    Gtc,
    /// Until a UTC time.
    GtdUnix(i64),
    /// Until a DAA score.
    GtdDaa(i64),
    /// Until the next 00:00 UTC.
    Day,
}

/// Timed activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// UTC unix seconds.
    Unix(i64),
    /// DAA score.
    Daa(i64),
}

/// What a limit that already crosses the book does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crossing {
    /// An auction from the touch to the limit (default).
    Auction,
    /// A plain limit: fills at its limit.
    Limit,
    /// Refuse.
    Reject,
}

/// A limit order (GTC / GTD / day, optional timed activation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitIntent {
    /// Side.
    pub side: Side,
    /// Base units.
    pub amount: i64,
    /// State price.
    pub price: i64,
    /// Tip.
    pub tip: i64,
    /// Minimum fill (`None`: the wallet default).
    pub min_fill: Option<i64>,
    /// Lifetime.
    pub lifetime: Lifetime,
    /// Activation.
    pub active_from: Option<Activation>,
    /// Crossing policy.
    pub crossing: Crossing,
    /// Delivery carriers of a buy (default 3, at most the possible fills).
    pub max_fills: Option<i64>,
}

/// IOC / FOK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImmediateIntent {
    /// FOK (all or nothing) instead of IOC.
    pub fok: bool,
    /// Side.
    pub side: Side,
    /// Base units.
    pub amount: i64,
    /// State price.
    pub price: i64,
    /// Tip.
    pub tip: i64,
    /// Minimum fill (default 1).
    pub min_fill: Option<i64>,
    /// Life in DAA (default 300, at most 600).
    pub life_daa: Option<i64>,
    /// Activation.
    pub active_from: Option<Activation>,
}

/// A market order: an IOC (or FOK) auction from the touch to the slippage bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketIntent {
    /// Side.
    pub side: Side,
    /// Base units.
    pub amount: i64,
    /// Tip.
    pub tip: i64,
    /// Minimum fill (default 1).
    pub min_fill: Option<i64>,
    /// Bound in bps from the touch (default 300).
    pub slippage_bps: Option<i64>,
    /// FOK.
    pub all_or_nothing: bool,
}

/// The conditional kinds `kob place` offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondKind {
    /// Stop-market.
    StopMarket,
    /// Stop-limit.
    StopLimit,
    /// Take-profit.
    TakeProfit,
    /// OCO: take-profit and stop in one covenant.
    Oco,
}

/// A conditional order (stop-market, stop-limit, take-profit, OCO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CondIntent {
    /// Kind.
    pub kind: CondKind,
    /// Side.
    pub side: Side,
    /// Base units.
    pub amount: i64,
    /// Tip.
    pub tip: i64,
    /// Minimum fill.
    pub min_fill: Option<i64>,
    /// Trigger.
    pub stop: Option<i64>,
    /// Stop-limit's limit.
    pub limit: Option<i64>,
    /// Take-profit.
    pub take_profit: Option<i64>,
    /// Stop band (bps, default 300).
    pub slip_bps: Option<i64>,
    /// Trigger threshold (default: max(minimum fill, 25 % of the amount)).
    pub min_touch: Option<i64>,
    /// Lifetime.
    pub lifetime: Lifetime,
    /// Activation.
    pub active_from: Option<Activation>,
    /// Delivery carriers of a buy.
    pub max_fills: Option<i64>,
}

/// An if-done order: an entry whose fills create the committed exit (IFD: one leg, IFO: both).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfdIntent {
    /// Entry side: buy = buy first, sell = sell first.
    pub side: Side,
    /// Base units.
    pub amount: i64,
    /// Entry tip.
    pub tip: i64,
    /// Entry minimum fill (default ceil(amount / 4)).
    pub min_fill: Option<i64>,
    /// Entry limit price.
    pub price: i64,
    /// Exit take-profit.
    pub take_profit: Option<i64>,
    /// Exit stop.
    pub stop: Option<i64>,
    /// Exit stop-limit.
    pub stop_limit: Option<i64>,
    /// Exit tip.
    pub exit_tip: i64,
    /// Sell-first prefund per whole token (default: the smallest sufficient).
    pub prefund: Option<i64>,
    /// Entry lifetime.
    pub lifetime: Lifetime,
    /// Activation.
    pub active_from: Option<Activation>,
}

/// Every intent `kob place` plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Limit on the KAS book.
    Limit(LimitIntent),
    /// IOC / FOK.
    Immediate(ImmediateIntent),
    /// Market.
    Market(MarketIntent),
    /// Conditional.
    Cond(CondIntent),
    /// If-done (IFD / IFO).
    Ifd(IfdIntent),
    /// Limit on a pair A/B (`env.pair`): prices in B per whole A.
    PairLimit(LimitIntent),
}

/// A line of KAS the order locks (or keeps, `kept`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierLine {
    /// What it is.
    pub kind: &'static str,
    /// Sompi per item.
    pub amount: i128,
    /// Items.
    pub count: i64,
    /// Stays in a UTXO of the wallet.
    pub kept: bool,
}

/// The numbers a confirmation shows (`Disclosure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    /// Side.
    pub side: Side,
    /// Base units traded.
    pub amount: i64,
    /// Order scale.
    pub scale: i64,
    /// Minimum fill.
    pub min_fill: i64,
    /// Trigger threshold.
    pub min_touch: Option<i64>,
    /// Limit (or worst bound).
    pub limit_price: Option<i64>,
    /// All-in price per whole token (a pair: the limit).
    pub all_in_price: Option<i64>,
    /// All-in total of the whole amount at the limit (a sell receives at least, a buy pays at most; a pair: in B).
    pub all_in_total: Option<i64>,
    /// Expected (the touch, an auction start).
    pub expected_price: Option<i64>,
    /// Worst.
    pub worst_price: Option<i64>,
    /// Tip.
    pub tip: i64,
    /// Lines.
    pub carriers: Vec<CarrierLine>,
    /// KAS locked into covenants.
    pub kas_locked: i128,
    /// Tokens moved into custody (a pair: A).
    pub tokens_escrowed: i64,
    /// B moved into custody (a pair).
    pub quote_escrowed: i64,
    /// Network fee.
    pub fee: u64,
    /// Refund tip.
    pub refund_tip: i64,
    /// Keeper tip.
    pub keeper_tip: i64,
    /// `gtc` / `gtd` / `day` / `ioc` / `fok`.
    pub expiry_kind: &'static str,
    /// Expiry DAA.
    pub expiry_daa: i64,
    /// Expiry around this UTC time.
    pub expiry_unix: i64,
    /// Day-order deadline.
    pub deadline: Option<i64>,
    /// Timed activation (DAA, approximate unix).
    pub activates_at: Option<(i64, i64)>,
    /// Sentence tags.
    pub notes: Vec<&'static str>,
}

/// A planned order.
#[derive(Debug, Clone)]
pub struct Plan {
    /// No errors and built.
    pub ok: bool,
    /// Findings.
    pub issues: Vec<Issue>,
    /// The orders after the transaction (an if-done entry lists its committed exit last).
    pub states: Vec<AnyState>,
    /// The `createOrder` request.
    pub request: Option<CreateOrder>,
    /// The unsigned transaction.
    pub built: Option<BuiltTx>,
    /// What to show.
    pub disclosure: Option<Disclosure>,
}

fn failed(issues: Vec<Issue>) -> Plan {
    Plan { ok: false, issues, states: vec![], request: None, built: None, disclosure: None }
}

// ------------------------------------------------------------------------------------------------ integer helpers

fn ceil_div(a: i128, b: i128) -> i128 {
    (a + b - 1) / b
}
fn round_div(a: i128, b: i128) -> i128 {
    (2 * a + b) / (2 * b)
}
fn fits(v: i128) -> bool {
    (0..=i64::MAX as i128).contains(&v)
}

/// Rounds to a multiple of `tick`.
fn round_to_tick(price: i64, tick: i64, up: bool) -> i64 {
    if tick <= 0 {
        return price;
    }
    let p = price as i128;
    let t = tick as i128;
    (if up { ceil_div(p, t) } else { p / t } * t) as i64
}

// ------------------------------------------------------------------------------------------------ time (daa.ts)

/// The rate in milli-DAA per second within the accepted range.
pub fn clamp_rate(r: i64) -> i64 {
    r.clamp(9_500, 10_500)
}
fn seconds_to_daa(c: &Clock, s: i64) -> i64 {
    ceil_div(s as i128 * clamp_rate(c.rate_milli) as i128, 1000) as i64
}
fn daa_to_seconds(c: &Clock, d: i64) -> i64 {
    round_div(d as i128 * 1000, clamp_rate(c.rate_milli) as i128) as i64
}
/// Wall clock at a DAA score.
pub fn daa_to_unix(c: &Clock, daa: i64) -> i64 {
    let delta = daa - c.daa;
    if delta >= 0 {
        c.unix + daa_to_seconds(c, delta)
    } else {
        c.unix - daa_to_seconds(c, -delta)
    }
}
fn unix_to_daa(c: &Clock, unix: i64) -> i64 {
    let delta = unix - c.unix;
    if delta >= 0 {
        c.daa + seconds_to_daa(c, delta)
    } else {
        c.daa - seconds_to_daa(c, -delta)
    }
}
/// The DAA score of a date on the side of the intent: the earlier of the measured and nominal rate for an end, the later
/// for a start.
pub fn unix_to_daa_bound(c: &Clock, unix: i64, end: bool) -> i64 {
    let measured = unix_to_daa(c, unix);
    let nominal = unix_to_daa(&Clock { rate_milli: 10_000, ..*c }, unix);
    if unix <= c.unix {
        return measured;
    }
    if end {
        measured.min(nominal)
    } else {
        measured.max(nominal)
    }
}

struct Expiry {
    kind: &'static str,
    daa: i64,
    deadline: Option<i64>,
}

fn activation_daa(c: &Clock, a: Option<Activation>) -> Option<i64> {
    a.map(|a| match a {
        Activation::Daa(d) => d,
        Activation::Unix(u) => unix_to_daa_bound(c, u, false),
    })
}

fn resolve_lifetime(c: &Clock, lt: Lifetime, pre: &mut Vec<Issue>) -> Expiry {
    match lt {
        Lifetime::Gtc => Expiry { kind: "gtc", daa: c.daa + MAX_IDLE_DAA, deadline: None },
        Lifetime::GtdUnix(at) => Expiry { kind: "gtd", daa: unix_to_daa_bound(c, at, true), deadline: None },
        Lifetime::GtdDaa(d) => Expiry { kind: "gtd", daa: d, deadline: None },
        Lifetime::Day => {
            let d = day_order(c.daa as u64, c.unix as u64, Some(clamp_rate(c.rate_milli) as u64));
            let left = (c.unix / 86_400 + 1) * 86_400 - c.unix;
            if left < 300 {
                pre.push(warning("DAY_ORDER_ENDS_SOON", format!("this day order ends by 00:00 UTC, within {} minutes", left / 60)));
            }
            Expiry { kind: "day", daa: d.expiry_daa as i64, deadline: Some(d.deadline as i64) }
        }
    }
}

// ------------------------------------------------------------------------------------------------ guards (guards.ts)

fn crosses(sell: (i64, i64), buy: (i64, i64)) -> bool {
    buy.0 as i128 + buy.1 as i128 >= sell.0 as i128 - sell.1 as i128
}

/// Best opposite price for an order on `side`.
pub fn touch_price(book: &Book, side: Side) -> Option<i64> {
    match side {
        Side::Sell => book.bids.first().map(|l| l.price),
        Side::Buy => book.asks.first().map(|l| l.price),
    }
}

/// Midpoint of the touch (or the one side that exists).
pub fn reference_price(book: &Book) -> Option<i64> {
    match (book.bids.first(), book.asks.first()) {
        (Some(b), Some(a)) => Some(((a.price as i128 + b.price as i128) / 2) as i64),
        (Some(b), None) => Some(b.price),
        (None, Some(a)) => Some(a.price),
        (None, None) => None,
    }
}

fn crossing_touch(book: &Book, side: Side, price: i64, tip: i64) -> Option<i64> {
    let lv = match side {
        Side::Sell => book.bids.first(),
        Side::Buy => book.asks.first(),
    }?;
    let mine = (price, tip);
    let theirs = (lv.price, lv.tip);
    let x = match side {
        Side::Sell => crosses(mine, theirs),
        Side::Buy => crosses(theirs, mine),
    };
    x.then_some(lv.price)
}

fn depth_within(book: &Book, side: Side, bound: i64, tip: i64) -> i128 {
    let levels = if side == Side::Sell { &book.bids } else { &book.asks };
    levels
        .iter()
        .filter(|l| match side {
            Side::Sell => l.price as i128 + l.tip as i128 >= bound as i128 - tip as i128,
            Side::Buy => l.price as i128 - l.tip as i128 <= bound as i128 + tip as i128,
        })
        .map(|l| l.amount as i128)
        .sum()
}

fn check_self_trade(own: &[OwnOrder], side: Side, price: i64, tip: i64) -> Vec<Issue> {
    let mut out = vec![];
    for o in own {
        if !o.active || o.side == side || o.amount_left <= 0 {
            continue;
        }
        let (sell, buy) = if side == Side::Sell { ((price, tip), (o.price, o.tip)) } else { ((o.price, o.tip), (price, tip)) };
        if crosses(sell, buy) {
            out.push(error(
                "SELF_TRADE",
                format!(
                    "this order would trade against your own resting order {} (price {}): cancel it first or change the price",
                    kob_protocol::json::to_hex(&o.cov),
                    o.price
                ),
            ));
        }
    }
    out
}

fn check_fok(side: Side, amount: i64, limit: i64, tip: i64, slots: (usize, usize), book: &Book) -> Vec<Issue> {
    let sell = side == Side::Sell;
    let max_cp = if sell { slots.1 } else { slots.0.min(MAX_TOKEN_INPUTS) };
    let ok = |p: i64, t: i64| {
        if sell {
            p as i128 + t as i128 >= limit as i128 - tip as i128
        } else {
            p as i128 - t as i128 <= limit as i128 + tip as i128
        }
    };
    let detail = if sell { &book.bid_orders } else { &book.ask_orders };
    let (available, count, exact): (i128, usize, bool) = match detail {
        Some(d) => {
            let mut cross: Vec<&BookOrder> =
                d.iter().filter(|o| o.amount > 0 && ok(o.price, o.tip) && o.min_fill.unwrap_or(1) <= amount).collect();
            cross.sort_by(|a, b| b.amount.cmp(&a.amount));
            let available = cross.iter().map(|o| o.amount as i128).sum();
            let (mut need, mut n) = (amount as i128, 0);
            for o in cross {
                if need <= 0 {
                    break;
                }
                need -= o.amount as i128;
                n += 1;
            }
            (available, n, true)
        }
        None => {
            let levels = if sell { &book.bids } else { &book.asks };
            let cross: Vec<&Level> = levels.iter().filter(|l| l.amount > 0 && ok(l.price, l.tip)).collect();
            let available = cross.iter().map(|l| l.amount as i128).sum();
            let (mut need, mut n) = (amount as i128, 0usize);
            for l in cross {
                if need <= 0 {
                    break;
                }
                need -= l.amount as i128;
                n += l.orders.max(0) as usize;
            }
            (available, n, false)
        }
    };
    if available < amount as i128 {
        return vec![error(
            "FOK_INSUFFICIENT_DEPTH",
            format!("fill-or-kill needs {amount} base units but only {available} cross at your price: it could not fill"),
        )];
    }
    if count > max_cp {
        if !sell && slots.0 > MAX_TOKEN_INPUTS && count > MAX_TOKEN_INPUTS {
            return vec![error(
                "FOK_TOO_MANY_TOKEN_INPUTS",
                format!("filling in one transaction would need {count} token inputs, more than 8"),
            )];
        }
        return vec![error(
            "FOK_TOO_MANY_COUNTERPARTIES",
            format!(
                "filling in one transaction would need {}{count} counterparties, more than the token allows ({max_cp}): use IOC or a smaller size",
                if exact { "" } else { "up to " }
            ),
        )];
    }
    vec![]
}

fn check_amount(amount: i64, m: &TokenMarket, custody: bool) -> Vec<Issue> {
    if amount <= 0 {
        return vec![error("AMOUNT_NOT_POSITIVE", "the amount must be greater than zero")];
    }
    if custody && m.family == Family::Kron && amount > KRON_MAX_OUTPUT_AMOUNT {
        return vec![error("AMOUNT_TOO_LARGE", format!("a KRON custody holds at most {KRON_MAX_OUTPUT_AMOUNT} base units"))];
    }
    vec![]
}

fn check_min_fill(min_fill: i64, amount: i64, field: &str) -> Vec<Issue> {
    if min_fill < 1 || min_fill > amount {
        return vec![error("MIN_FILL_INVALID", format!("{field} must be at least one base unit and at most the order amount"))];
    }
    vec![]
}

fn check_notional(amount: i64, rate: i64, scale: i64) -> Vec<Issue> {
    if amount <= 0 || rate <= 0 {
        return vec![];
    }
    if ceil_div(amount as i128 * rate as i128, scale as i128) >= QUOTE_LIMIT {
        return vec![error("NOTIONAL_TOO_LARGE", "the order value at this price is too large for the protocol (2^62)")];
    }
    vec![]
}

fn check_price(m: &TokenMarket, price: i64, field: &str) -> Vec<Issue> {
    if price <= 0 {
        return vec![error("PRICE_NOT_POSITIVE", format!("{field} must be greater than zero"))];
    }
    if m.tick > 0 && price % m.tick != 0 {
        return vec![error(
            "PRICE_NOT_ON_TICK",
            format!(
                "{field} must be a multiple of the tick ({} sompi per token); nearest valid prices: {} / {}",
                m.tick,
                round_to_tick(price, m.tick, false),
                round_to_tick(price, m.tick, true)
            ),
        )];
    }
    vec![]
}

fn check_tip(side: Side, tip: i64, worst: i64) -> Vec<Issue> {
    if tip < 0 {
        return vec![error("TIP_NEGATIVE", "the priority tip cannot be negative")];
    }
    if side == Side::Sell && tip >= worst {
        return vec![error("TIP_EXCEEDS_PRICE", format!("the priority tip ({tip} per token) must be smaller than the price"))];
    }
    vec![]
}

fn check_times(c: &Clock, active_from: i64, expiry: i64, requested: Option<i64>) -> Vec<Issue> {
    let mut out = vec![];
    let horizon = c.daa + MAX_IDLE_DAA;
    if requested.is_some_and(|r| r > 0 && r <= c.daa) {
        out.push(info("ACTIVE_IN_PAST", "the activation time has passed: the order activates immediately"));
    }
    if active_from > horizon {
        out.push(error("ACTIVE_TOO_FAR", "the activation time must be within 90 days"));
    }
    if expiry > horizon {
        out.push(error("EXPIRY_TOO_FAR", "an order can live at most 90 days"));
    }
    if expiry <= c.daa {
        out.push(error("EXPIRY_TOO_SOON", "the expiry must be later than the moment the order becomes active"));
    } else if expiry <= active_from {
        out.push(error("ACTIVE_AFTER_EXPIRY", "the order would expire before it activates"));
    }
    out
}

fn check_price_band(reference: Option<i64>, side: Side, price: i64) -> Vec<Issue> {
    let Some(r) = reference.filter(|r| *r > 0) else { return vec![] };
    let through = if side == Side::Sell { r as i128 - price as i128 } else { price as i128 - r as i128 };
    let pct = |bps: i128| format!("{}.{:02}", bps / 100, bps % 100);
    if through > 0 {
        let bps = through * 10_000 / r as i128;
        if bps >= 1_000 {
            return vec![warning(
                "PRICE_AGGRESSIVE_VS_MARKET",
                format!("your price is {}% through the market ({r}): check for a typing error", pct(bps)),
            )];
        }
        return vec![];
    }
    let bps = -through * 10_000 / r as i128;
    if bps >= 5_000 {
        return vec![warning(
            "PRICE_FAR_FROM_MARKET",
            format!("your price is {}% away from the market ({r}): it is unlikely to fill soon", pct(bps)),
        )];
    }
    vec![]
}

fn check_bps(bps: i64, pre: &mut Vec<Issue>) {
    if !(1..=9_999).contains(&bps) {
        pre.push(error("SLIPPAGE_INVALID", "the slippage tolerance must be between 0.01% and 99.99%"));
    } else if bps > 1_000 {
        pre.push(warning("SLIPPAGE_HIGH", format!("a slippage tolerance of {bps} bps lets the order fill far from the market price")));
    }
}

/// Worst price of an auction from `touch`.
pub fn auction_bound(side: Side, touch: i64, bps: i64, tick: i64) -> i64 {
    let mv = (touch as i128 * bps as i128 / 10_000) as i64;
    match side {
        Side::Sell => round_to_tick(touch - mv, tick, true).min(touch),
        Side::Buy => round_to_tick(touch + mv, tick, false).max(touch),
    }
}

/// Slope reaching `to` from `from` in `duration` DAA at `step` DAA per step (rounded up).
pub fn decay_slope(from: i64, to: i64, duration: i64, step: i64) -> i64 {
    let diff = (from as i128 - to as i128).abs();
    if diff == 0 {
        return 0;
    }
    ceil_div(diff, ceil_div(duration as i128, step as i128)) as i64
}

// ------------------------------------------------------------------------------------------------ coin selection (funding.ts)

fn outpoint_key(u: &kob_protocol::tx::Utxo) -> ([u8; 32], u32) {
    (u.transaction_id, u.index)
}

/// Largest-first plain KAS UTXOs covering `need + reserve` (at least one input).
pub fn select_funding(utxos: &[KeyUtxo], need: i128, reserve: i128) -> Result<Vec<KeyUtxo>, Issue> {
    let target = need.max(0) + reserve.max(0);
    let mut sorted: Vec<&KeyUtxo> = utxos.iter().filter(|u| u.utxo.covenant_id.is_none() && u.utxo.amount > 0).collect();
    sorted.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount).then(outpoint_key(&a.utxo).cmp(&outpoint_key(&b.utxo))));
    let have: i128 = sorted.iter().map(|u| u.utxo.amount as i128).sum();
    if have < target || sorted.is_empty() {
        return Err(insufficient_kas(target, have));
    }
    let mut picked = vec![];
    let mut sum = 0i128;
    for u in sorted {
        picked.push(u.clone());
        sum += u.utxo.amount as i128;
        if sum >= target {
            break;
        }
    }
    if picked.len() > MAX_FUNDING_INPUTS {
        return Err(error(
            "FUNDING_FRAGMENTED",
            format!(
                "your KAS is spread over too many UTXOs: one transaction can spend at most {MAX_FUNDING_INPUTS}; consolidate first"
            ),
        ));
    }
    Ok(picked)
}

fn insufficient_kas(needed: i128, have: i128) -> Issue {
    error(
        "INSUFFICIENT_KAS",
        format!(
            "not enough KAS: this needs {} sompi, the spendable balance is {have} sompi (short by {})",
            needed,
            (needed - have).max(0)
        ),
    )
}

/// The token UTXOs covering `amount`: the smallest single one that suffices, else the largest first, within `max_inputs`.
pub fn select_tokens(utxos: &[TokenUtxo], amount: i64, max_inputs: usize, label: &str) -> Result<Vec<TokenUtxo>, Issue> {
    let usable: Vec<&TokenUtxo> = utxos.iter().filter(|u| u.state.is_user() && u.state.amount() > 0).collect();
    let have: i128 = usable.iter().map(|u| u.state.amount() as i128).sum();
    if have < amount as i128 {
        return Err(error(
            "INSUFFICIENT_TOKENS",
            format!("not enough {label}: this needs {amount} base units, you hold {have} (short by {})", amount as i128 - have),
        ));
    }
    let mut asc = usable.clone();
    asc.sort_by(|a, b| a.state.amount().cmp(&b.state.amount()).then(outpoint_key(&a.utxo).cmp(&outpoint_key(&b.utxo))));
    if let Some(u) = asc.iter().find(|u| u.state.amount() >= amount) {
        return Ok(vec![(*u).clone()]);
    }
    let mut picked = vec![];
    let mut sum = 0i128;
    for u in asc.iter().rev() {
        picked.push((*u).clone());
        sum += u.state.amount() as i128;
        if sum >= amount as i128 {
            break;
        }
    }
    if picked.len() > max_inputs {
        return Err(error(
            "TOKEN_UTXOS_FRAGMENTED",
            format!("your {label} are spread over too many UTXOs: one transaction can spend at most {max_inputs}; merge them first"),
        ));
    }
    Ok(picked)
}

/// The maker's key-owned plain UTXOs of `m` (same extension commitment for KCC-20).
pub fn usable_token_utxos(m: &TokenMarket, maker: &[u8; 32], utxos: &[TokenUtxo]) -> Vec<TokenUtxo> {
    utxos
        .iter()
        .filter(|u| {
            u.utxo.covenant_id == Some(m.cov)
                && u.state.is_user()
                && u.state.owner() == *maker
                && u.state.is_plain()
                && u.state.family() == m.family
                && (m.family == Family::Kron || u.state.extension() == m.ext)
        })
        .cloned()
        .collect()
}

/// The least KAS a token output of `m`'s program carries: its own floor and the dust bound.
pub fn token_carrier_floor(m: &TokenMarket) -> i64 {
    m.program.min_token_output().unwrap_or(1).max(DUST_OUTPUT_MIN) as i64
}

/// The least KAS a carrier of a new order holds when it becomes a token output of `m`'s program (its custody, a delivery):
/// the token output's floor and the KIP-9 bound of a covenant output (`tx::COVENANT_OUTPUT_MIN`, matcher.md 10.1).
pub fn order_carrier_floor(m: &TokenMarket) -> i64 {
    token_carrier_floor(m).max(COVENANT_OUTPUT_MIN as i64)
}

// ------------------------------------------------------------------------------------------------ building (common.ts)

struct CreateSpec {
    order: AnyState,
    value: i64,
    token_amount: i64,
    deadline: Option<i64>,
}

struct CreateResult {
    request: Option<CreateOrder>,
    built: Option<BuiltTx>,
    token_carrier: i64,
    token_change: i64,
    issues: Vec<Issue>,
}

fn fee_options(env: &Env) -> FeeOptions {
    FeeOptions { fee_rate: env.fee_rate, ..Default::default() }
}

fn carrier_issues(env: &Env, order: &AnyState, token_carrier: i64) -> Vec<Issue> {
    let m = &env.market;
    let mut out = vec![];
    let mut check = |field: &str, v: i64, token: bool| {
        // every carrier of a new order becomes an output with a covenant id (a token output, an exit order UTXO)
        let floor = if token { order_carrier_floor(m) } else { COVENANT_OUTPUT_MIN as i64 };
        if v < floor {
            out.push(error(
                "CARRIER_BELOW_FLOOR",
                format!("the {field} ({v} sompi) is below {floor} sompi, the least this output can carry"),
            ));
        }
    };
    if token_carrier > 0 {
        check("carrier", token_carrier, true);
    }
    match order {
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => check("delivery carrier", s.delivery_carrier, true),
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => check("delivery carrier", s.delivery_carrier, true),
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
            check("delivery carrier", s.delivery_carrier, true);
            check("exit carrier", s.exit_carrier, false);
        }
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => check("exit carrier", s.exit_carrier, true),
        _ => {}
    }
    out
}

fn build_create(env: &Env, spec: &CreateSpec) -> CreateResult {
    let mut res = CreateResult { request: None, built: None, token_carrier: 0, token_change: 0, issues: vec![] };
    let carrier = env.carrier();
    let low = carrier_issues(env, &spec.order, if spec.token_amount > 0 { carrier } else { 0 });
    if !low.is_empty() {
        res.issues = low;
        return res;
    }
    let mut tokens = vec![];
    let mut need = spec.value as i128;
    if spec.token_amount > 0 {
        let usable = usable_token_utxos(&env.market, &env.maker, &env.token_utxos);
        match select_tokens(&usable, spec.token_amount, env.market.slots.0.min(MAX_TOKEN_INPUTS), &env.market.name()) {
            Ok(t) => tokens = t,
            Err(e) => {
                res.issues.push(e);
                return res;
            }
        }
        res.token_carrier = carrier;
        let have: i128 = tokens.iter().map(|u| u.state.amount() as i128).sum();
        res.token_change = (have - spec.token_amount as i128) as i64;
        let inputs_kas: i128 = tokens.iter().map(|u| u.utxo.amount as i128).sum();
        need = spec.value as i128 + carrier as i128 + if res.token_change > 0 { carrier as i128 } else { 0 };
        need = (need - inputs_kas).max(0);
    }
    let request = |funding: Vec<KeyUtxo>| CreateOrder {
        order: spec.order.clone(),
        value: spec.value as u64,
        tokens: tokens.clone(),
        token_carrier: if tokens.is_empty() { 0 } else { carrier as u64 },
        funding,
        change: None,
        lock_time: 0,
        deadline: spec.deadline.map(|d| d as u64),
        records: vec![],
        fee: fee_options(env),
    };
    let token_draws = vec![(spec.order.token_cov_id(), spec.token_amount)];
    match build_with_reserve(env, need, request, |b| verify_built(b, &spec.order, spec.value, spec.deadline, &token_draws)) {
        Ok((req, built)) => {
            res.request = Some(req);
            res.built = Some(built);
        }
        Err(e) => res.issues.push(e),
    }
    res
}

/// Funding selection with an adaptive fee reserve: the reserve grows by the shortfall the builder reports.
fn build_with_reserve(
    env: &Env,
    need: i128,
    request: impl Fn(Vec<KeyUtxo>) -> CreateOrder,
    verify: impl Fn(&BuiltTx) -> Option<String>,
) -> Result<(CreateOrder, BuiltTx), Issue> {
    let balance: i128 = env.funding.iter().filter(|u| u.utxo.covenant_id.is_none()).map(|u| u.utxo.amount as i128).sum();
    if balance < need {
        return Err(insufficient_kas(need, balance));
    }
    let mut reserve = DEFAULT_FEE_RESERVE;
    for _ in 0..5 {
        let room = balance - need;
        let r = reserve.min(room);
        let funding = select_funding(&env.funding, need, r)?;
        let req = request(funding);
        match build(&Action::CreateOrder(req.clone())) {
            Ok(built) => {
                if let Some(why) = verify(&built) {
                    return Err(error("PLAN_MISMATCH", format!("the built transaction is not the planned order: {why}")));
                }
                return Ok((req, built));
            }
            Err(kob_protocol::Error::InsufficientFunds { need: n, have: h }) => {
                let short = n as i128 - h as i128;
                if r >= room {
                    return Err(insufficient_kas(balance + short, balance));
                }
                reserve = r + short + 1_000_000;
            }
            Err(e) => return Err(error("BUILD_REJECTED", format!("the protocol refused this order: {e}"))),
        }
    }
    Err(error("BUILD_REJECTED", "could not size the network fee"))
}

/// The placement record of `built` must yield the planned state, value, deadline and every custody amount.
fn verify_built(built: &BuiltTx, order: &AnyState, value: i64, deadline: Option<i64>, draws: &[([u8; 32], i64)]) -> Option<String> {
    let rec = match recover_orders(&built.tx) {
        Ok(r) => r,
        Err(e) => return Some(format!("placement record does not verify: {e}")),
    };
    let Some(r) = rec.iter().find(|x| x.output == 0).or(rec.first()) else { return Some("no placement record".into()) };
    if r.order.try_encode().ok() != order.try_encode().ok() {
        return Some("recovered order state differs".into());
    }
    if r.value != value as u64 {
        return Some("recovered order value differs".into());
    }
    if r.deadline.map(|d| d as i64) != deadline {
        return Some("recovered deadline differs".into());
    }
    let parts = [r.custody.as_ref(), r.prefund.as_ref()];
    let wanted: Vec<&([u8; 32], i64)> = draws.iter().filter(|d| d.1 > 0).collect();
    for (k, got) in parts.iter().enumerate() {
        match (wanted.get(k), got) {
            (None, None) => {}
            (None, Some(_)) => return Some("an unexpected custody was created".into()),
            (Some(_), None) => return Some("a custody is missing".into()),
            (Some((token, amount)), Some(g)) => {
                if g.state.amount() != *amount {
                    return Some("custody amount differs".into());
                }
                let out = built.tx.outputs.get(g.output as usize);
                if out.and_then(|o| o.covenant.as_ref()).map(|c| c.covenant_id) != Some(*token) {
                    return Some("a custody is not an output of its token".into());
                }
                if g.state.is_user() || g.state.owner() != r.covenant_id {
                    return Some("a custody is not owned by the new order".into());
                }
            }
        }
    }
    None
}

// ------------------------------------------------------------------------------------------------ plain kinds (simple.ts)

fn make_ask(env: &Env, p: &Draft, min_fill: i64) -> AnyState {
    let m = &env.market;
    let s = AskState {
        maker: env.maker,
        token_cov_id: m.cov,
        token_tpl_hash: m.tpl_hash,
        tpl_prefix_len: m.prefix_len,
        tpl_suffix_len: m.suffix_len,
        scale: m.scale,
        min_fill,
        price: p.price,
        tip: p.tip,
        tif: p.tif,
        active_from: p.active_from,
        expiry_daa: p.expiry_daa,
        refund_tip: m.refund_tip,
        interval: p.interval,
        max_fill: p.max_fill,
        slope: p.slope,
        price_end: p.price_end,
        decay_step: p.decay_step,
        amount_left: p.amount,
        extension_commitment: m.ext,
    };
    match m.family {
        Family::Kcc20 => AnyState::KobAsk(s),
        Family::Kron => AnyState::KobAskKron(s),
    }
}

fn make_bid(env: &Env, p: &Draft, min_fill: i64) -> AnyState {
    let m = &env.market;
    let s = BidState {
        maker: env.maker,
        token_cov_id: m.cov,
        token_tpl_hash: m.tpl_hash,
        tpl_prefix_len: m.prefix_len,
        tpl_suffix_len: m.suffix_len,
        extension_commitment: m.ext,
        scale: m.scale,
        min_fill,
        price: p.price,
        tip: p.tip,
        tif: p.tif,
        active_from: p.active_from,
        expiry_daa: p.expiry_daa,
        refund_tip: m.refund_tip,
        reserve: 0,
        delivery_carrier: env.carrier(),
        interval: p.interval,
        max_fill: p.max_fill,
        slope: p.slope,
        price_end: p.price_end,
        decay_step: p.decay_step,
    };
    match m.family {
        Family::Kcc20 => AnyState::KobBid(s),
        Family::Kron => AnyState::KobBidKron(s),
    }
}

/// A resolved plain order (simple.ts `Draft`).
#[derive(Debug, Clone)]
struct Draft {
    side: Side,
    amount: i64,
    min_fill: Option<i64>,
    immediate: bool,
    price: i64,
    tip: i64,
    tif: i64,
    active_from: i64,
    requested_active_from: Option<i64>,
    expiry_daa: i64,
    interval: i64,
    max_fill: i64,
    slope: i64,
    price_end: i64,
    decay_step: i64,
    deadline: Option<i64>,
    fills: Option<i64>,
    fills_requested: Option<i64>,
    reach: i64,
    expiry_kind: &'static str,
    expected_price: Option<i64>,
    worst_price: Option<i64>,
    limit_price: i64,
    activates_at: Option<i64>,
    notes: Vec<&'static str>,
}

fn blank(side: Side, amount: i64, price: i64, tip: i64, min_fill: Option<i64>) -> Draft {
    Draft {
        side,
        amount,
        min_fill,
        immediate: false,
        price,
        tip,
        tif: 0,
        active_from: 0,
        requested_active_from: None,
        expiry_daa: 0,
        interval: 0,
        max_fill: 0,
        slope: 0,
        price_end: 0,
        decay_step: 1,
        deadline: None,
        fills: Some(1),
        fills_requested: None,
        reach: price,
        expiry_kind: "gtc",
        expected_price: None,
        worst_price: None,
        limit_price: price,
        activates_at: None,
        notes: vec![],
    }
}

/// The wallet's default minimum fill of a resting order: the amount worth 10 KAS at `price`.
pub fn default_min_fill_for(m: &TokenMarket, amount: i64, price: i64) -> i64 {
    if amount <= 0 {
        1
    } else {
        default_min_fill(amount, price.max(0), m.scale)
    }
}

/// Resting bids budget a delivery carrier per expected fill, at most 3, unless the user asked.
fn resting_fills(m: &TokenMarket, amount: i64, price: i64, min_fill: Option<i64>, requested: Option<i64>) -> i64 {
    if let Some(r) = requested {
        return r;
    }
    let mf = min_fill.unwrap_or_else(|| default_min_fill_for(m, amount, price));
    let possible = if mf > 0 && amount > 0 { ceil_div(amount as i128, mf as i128) as i64 } else { 1 };
    possible.min(DEFAULT_BID_FILLS)
}

fn place_simple(env: &Env, d: Draft, mut issues: Vec<Issue>) -> Plan {
    let m = &env.market;
    issues.extend(check_amount(d.amount, m, d.side == Side::Sell));
    issues.extend(check_tip(d.side, d.tip, d.reach));
    issues.extend(check_times(&env.clock, d.active_from, d.expiry_daa, d.requested_active_from));
    if has_error(&issues) {
        return failed(issues);
    }
    let mut min_fill = d.min_fill.unwrap_or_else(|| if d.immediate { 1 } else { default_min_fill_for(m, d.amount, d.limit_price) });
    if d.min_fill.is_none() && d.max_fill > 0 && min_fill > d.max_fill {
        min_fill = d.max_fill;
    }
    issues.extend(check_min_fill(min_fill, d.amount, "the minimum fill"));
    let top = d.price.max(d.price_end);
    issues.extend(check_notional(d.amount, if d.side == Side::Sell { top } else { top.saturating_add(d.tip) }, m.scale));
    if has_error(&issues) {
        return failed(issues);
    }
    if d.tif == 2 {
        issues.extend(check_fok(d.side, d.amount, d.reach, d.tip, m.slots, &env.book));
    }
    issues.extend(check_self_trade(&env.own, d.side, d.reach, d.tip));
    if has_error(&issues) {
        return failed(issues);
    }
    let fills = d.fills.unwrap_or(1);
    let (order, value, token_amount) = match d.side {
        Side::Sell => (make_ask(env, &d, min_fill), env.carrier(), d.amount),
        Side::Buy => {
            let o = make_bid(env, &d, min_fill);
            let escrow = match &o {
                AnyState::KobBid(b) | AnyState::KobBidKron(b) => b.escrow(d.amount, fills),
                _ => None,
            };
            let Some(escrow) = escrow else {
                issues.push(error("AMOUNT_TOO_LARGE", "the amount is too large for one order"));
                return failed(issues);
            };
            (o, escrow, 0)
        }
    };
    let res = build_create(env, &CreateSpec { order: order.clone(), value, token_amount, deadline: d.deadline });
    issues.extend(res.issues.clone());
    let Some(built) = res.built.clone() else { return Plan { states: vec![order], ..failed(issues) } };
    // disclosure (makeDisclosure)
    let mut carriers = vec![];
    let (tip, refund_tip, all_in_total);
    match &order {
        AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
            tip = s.tip;
            refund_tip = s.refund_tip;
            all_in_total = s.proceeds_at(d.amount, d.limit_price);
            carriers.push(CarrierLine { kind: "order carrier", amount: value as i128, count: 1, kept: false });
            carriers.push(CarrierLine { kind: "custody carrier", amount: res.token_carrier as i128, count: 1, kept: false });
            if res.token_change > 0 {
                carriers.push(CarrierLine { kind: "token change carrier", amount: res.token_carrier as i128, count: 1, kept: true });
            }
        }
        AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
            tip = s.tip;
            refund_tip = s.refund_tip;
            let carrier = s.delivery_carrier as i128;
            carriers.push(CarrierLine {
                kind: "escrow",
                amount: value as i128 - carrier * fills as i128 - s.reserve as i128,
                count: 1,
                kept: false,
            });
            carriers.push(CarrierLine { kind: "delivery carrier", amount: carrier, count: fills, kept: false });
            all_in_total = s.spend_at(d.amount, d.limit_price);
        }
        _ => unreachable!("plain kinds"),
    }
    let kas_locked: i128 = carriers.iter().filter(|c| !c.kept).map(|c| c.amount * c.count as i128).sum();
    if all_in_total.is_some_and(|t| kas_locked > t as i128) {
        issues.push(warning(
            "CARRIERS_DOMINATE",
            format!("the KAS locked as carriers ({kas_locked} sompi) exceeds the order value; it is returned when the order ends"),
        ));
    }
    let all_in_price = Some(if d.side == Side::Sell { d.limit_price - tip } else { d.limit_price + tip });
    let disclosure = Disclosure {
        side: d.side,
        amount: d.amount,
        scale: m.scale,
        min_fill,
        min_touch: None,
        limit_price: Some(d.limit_price),
        all_in_price,
        all_in_total,
        expected_price: d.expected_price,
        worst_price: d.worst_price,
        tip,
        carriers,
        kas_locked,
        tokens_escrowed: token_amount,
        quote_escrowed: 0,
        fee: built.fee.fee,
        refund_tip,
        keeper_tip: 0,
        expiry_kind: d.expiry_kind,
        expiry_daa: d.expiry_daa,
        expiry_unix: daa_to_unix(&env.clock, d.expiry_daa),
        deadline: d.deadline,
        activates_at: d.activates_at.filter(|a| *a > env.clock.daa).map(|a| (a, daa_to_unix(&env.clock, a))),
        notes: d.notes.clone(),
    };
    Plan {
        ok: !has_error(&issues),
        issues,
        states: vec![order],
        request: res.request,
        built: Some(built),
        disclosure: Some(disclosure),
    }
}

fn plan_limit(env: &Env, i: &LimitIntent) -> Plan {
    let m = &env.market;
    let mut pre = check_price(m, i.price, "the price");
    if i.max_fills.is_some_and(|f| f < 1) {
        pre.push(error("MAX_FILLS_INVALID", "the number of fills must be a positive integer"));
    }
    if has_error(&pre) {
        return failed(pre);
    }
    let mut d = blank(i.side, i.amount, i.price, i.tip, i.min_fill);
    let requested = activation_daa(&env.clock, i.active_from);
    let future = requested.is_some_and(|r| r > env.clock.daa);
    d.requested_active_from = requested;
    d.active_from = if future { requested.unwrap_or(0) } else { 0 };
    d.activates_at = if future { requested } else { None };
    let exp = resolve_lifetime(&env.clock, i.lifetime, &mut pre);
    d.expiry_daa = exp.daa;
    d.deadline = exp.deadline;
    d.expiry_kind = exp.kind;
    d.notes.push(match exp.kind {
        "gtc" => "gtc",
        "day" => "dayOrder",
        _ => "gtd",
    });
    d.fills = Some(if i.side == Side::Buy { resting_fills(m, i.amount, i.price, i.min_fill, i.max_fills) } else { 1 });
    if i.side == Side::Sell && i.max_fills.is_some() {
        pre.push(error("SIDE_INVALID", "--max-fills applies to a buy only"));
    }
    pre.extend(check_price_band(reference_price(&env.book), i.side, i.price));
    let touch = if future { None } else { crossing_touch(&env.book, i.side, i.price, i.tip) };
    if let Some(t) = touch {
        let start = if i.side == Side::Sell { t.max(i.price) } else { t.min(i.price) };
        match i.crossing {
            Crossing::Reject => pre.push(error("MARKETABLE_REJECTED", format!("your limit crosses the market at {t}"))),
            Crossing::Auction if start != i.price => {
                d.price = start;
                d.price_end = i.price;
                d.slope = decay_slope(start, i.price, MARKET_AUCTION_DAA, 1);
                d.decay_step = 1;
                d.active_from = env.clock.daa + MARKET_ACTIVATION_DAA;
                d.activates_at = None;
                d.reach = i.price;
                d.limit_price = i.price;
                d.expected_price = Some(t);
                d.worst_price = Some(i.price);
                d.notes.extend(["auction", "marketable"]);
                pre.push(info(
                    "MARKETABLE_AUCTION",
                    format!("your limit crosses the market: placed as an auction from the best price ({t}) to your limit; any rest stays at your limit"),
                ));
            }
            _ if start != i.price => pre.push(warning(
                "MARKETABLE_LIMIT_FILLS_AT_LIMIT",
                format!("your limit crosses the market: it fills at your limit price; a matcher keeps the difference to the better price ({t})"),
            )),
            _ => {}
        }
    }
    place_simple(env, d, pre)
}

fn life_daa(life: Option<i64>, pre: &mut Vec<Issue>) -> i64 {
    let v = life.unwrap_or(IOC_LIFE_DAA);
    if !(1..=IOC_KILL_DAA).contains(&v) {
        pre.push(error("LIFE_OUT_OF_RANGE", format!("the order life must be between 1 and {IOC_KILL_DAA} DAA")));
    }
    v
}

fn plan_immediate(env: &Env, i: &ImmediateIntent) -> Plan {
    let mut pre = check_price(&env.market, i.price, "the price");
    let life = life_daa(i.life_daa, &mut pre);
    if has_error(&pre) {
        return failed(pre);
    }
    let mut d = blank(i.side, i.amount, i.price, i.tip, i.min_fill);
    d.immediate = true;
    let requested = activation_daa(&env.clock, i.active_from);
    let future = requested.is_some_and(|r| r > env.clock.daa);
    d.requested_active_from = requested;
    d.active_from = if future { requested.unwrap_or(0) } else { 0 };
    d.activates_at = if future { requested } else { None };
    d.tif = if i.fok { 2 } else { 1 };
    d.expiry_kind = if i.fok { "fok" } else { "ioc" };
    d.expiry_daa = if future { requested.unwrap_or(0) } else { env.clock.daa } + life;
    d.fills = Some(1);
    d.notes.push(if i.fok { "fokAllOrNothing" } else { "iocRemainderReturned" });
    pre.extend(check_price_band(reference_price(&env.book), i.side, i.price));
    place_simple(env, d, pre)
}

fn plan_market(env: &Env, i: &MarketIntent) -> Plan {
    let mut pre = vec![];
    let bps = i.slippage_bps.unwrap_or(SLIPPAGE_BPS);
    check_bps(bps, &mut pre);
    let life = IOC_LIFE_DAA;
    let touch = touch_price(&env.book, i.side);
    if touch.is_none() {
        pre.push(error(
            "NO_LIQUIDITY",
            format!("there are no {} in the book to price a market order against", if i.side == Side::Sell { "bids" } else { "asks" }),
        ));
    }
    if has_error(&pre) {
        return failed(pre);
    }
    let touch = touch.unwrap_or(0);
    let end = auction_bound(i.side, touch, bps, env.market.tick);
    let mut d = blank(i.side, i.amount, touch, i.tip, i.min_fill);
    d.immediate = true;
    d.tif = if i.all_or_nothing { 2 } else { 1 };
    d.expiry_kind = if i.all_or_nothing { "fok" } else { "ioc" };
    d.active_from = env.clock.daa + MARKET_ACTIVATION_DAA;
    d.expiry_daa = d.active_from + life;
    d.slope = decay_slope(touch, end, MARKET_AUCTION_DAA, 1);
    d.price_end = if d.slope > 0 { end } else { 0 };
    d.decay_step = 1;
    d.reach = end;
    d.limit_price = end;
    d.expected_price = Some(touch);
    d.worst_price = Some(end);
    d.fills = Some(1);
    d.notes.extend(["auction", if i.all_or_nothing { "fokAllOrNothing" } else { "iocRemainderReturned" }, "market"]);
    pre.push(info(
        "MARKET_REFERENCE_SOURCE",
        format!("market reference: {touch} per token from the indexer order book; the order can fill at {end} per token at worst"),
    ));
    if !i.all_or_nothing {
        let depth = depth_within(&env.book, i.side, end, i.tip);
        if depth < i.amount as i128 {
            pre.push(warning(
                "MARKET_DEPTH_INSUFFICIENT",
                format!(
                    "the visible book within your bound holds only {depth} of your {} base units; the rest is returned unfilled",
                    i.amount
                ),
            ));
        }
    }
    place_simple(env, d, pre)
}

// ------------------------------------------------------------------------------------------------ conditional kinds (cond*.ts)

struct Legs {
    tp: i64,
    stop: i64,
    slip: i64,
    band: i64,
    keeper_tip: i64,
    min_touch: i64,
    min_rest: i64,
    stop_worst: i64,
    expected_updates: i64,
}

/// The largest `slipBps` whose band ends at or inside `limit`.
pub fn slip_for_limit(side: Side, stop: i64, limit: i64) -> i64 {
    if stop <= 0 {
        return 0;
    }
    let dist = if side == Side::Sell { stop as i128 - limit as i128 } else { limit as i128 - stop as i128 };
    if dist <= 0 {
        return 0;
    }
    (((dist + 1) * 10_000 - 1) / stop as i128).min(10_000) as i64
}

/// The stop leg's worst price.
pub fn stop_worst_price(side: Side, stop: i64, slip: i64) -> i64 {
    let mv = (stop as i128 * slip as i128 / 10_000) as i64;
    if side == Side::Sell {
        stop - mv
    } else {
        stop + mv
    }
}

struct LegInput {
    tp: Option<i64>,
    stop: Option<i64>,
    limit: Option<i64>,
    slip_bps: Option<i64>,
    min_touch: Option<i64>,
}

#[allow(clippy::too_many_arguments)]
fn resolve_legs(
    env: &Env,
    log: &mut Vec<Issue>,
    side: Side,
    input: &LegInput,
    f: &str,
    min_fill: i64,
    amount: i64,
    keeper_tip: i64,
) -> Option<Legs> {
    let m = &env.market;
    if input.tp.is_none() && input.stop.is_none() {
        log.push(error("COND_LEGS_MISSING", "give a take-profit or a stop"));
        return None;
    }
    let mut bad = vec![];
    if let Some(tp) = input.tp {
        bad.extend(check_price(m, tp, &format!("{f}take-profit")));
    }
    if let Some(s) = input.stop {
        bad.extend(check_price(m, s, &format!("{f}stop")));
    }
    if !bad.is_empty() {
        log.extend(bad);
        return None;
    }
    let tp = input.tp.unwrap_or(0);
    let stop = input.stop.unwrap_or(0);
    if input.stop.is_some() && stop > kob_protocol::state::MAX_STOP_PRICE {
        log.push(error("PRICE_TOO_LARGE", format!("{f}stop is too large")));
        return None;
    }
    if input.tp.is_some() && input.stop.is_some() && (if side == Side::Sell { tp <= stop } else { tp >= stop }) {
        log.push(error(
            "COND_TP_STOP_ORDER",
            format!("the {f}take-profit must be {} the stop", if side == Side::Sell { "above" } else { "below" }),
        ));
        return None;
    }
    let mut slip = input.slip_bps.unwrap_or(SLIPPAGE_BPS);
    if !(0..=10_000).contains(&slip) {
        log.push(error("COND_SLIP_INVALID", format!("{f}stop band must be 0..100%")));
        return None;
    }
    if let (Some(s), Some(lim)) = (input.stop, input.limit) {
        if lim <= 0 {
            log.push(error("PRICE_NOT_POSITIVE", format!("{f}limit must be greater than zero")));
            return None;
        }
        if if side == Side::Sell { lim > s } else { lim < s } {
            log.push(error(
                "COND_STOP_LIMIT_BEYOND_STOP",
                format!(
                    "the limit of a {} stop must be at or {} the stop",
                    side.name(),
                    if side == Side::Sell { "below" } else { "above" }
                ),
            ));
            return None;
        }
        slip = slip_for_limit(side, stop, lim);
    }
    let mut band = STOP_BAND_DAA;
    if input.stop.is_some() && slip == 0 {
        band = 0;
    }
    let stop_worst = if input.stop.is_some() { stop_worst_price(side, stop, slip) } else { 0 };
    if input.stop.is_some() && stop_worst <= 0 {
        log.push(error("COND_WORST_PRICE_INVALID", format!("{f}stop is too low for its price band")));
        return None;
    }
    let min_touch = input.min_touch.unwrap_or_else(|| default_min_touch(min_fill, amount));
    if min_touch <= 0 {
        log.push(error("COND_MIN_TOUCH_INVALID", format!("{f}trigger threshold must be positive")));
        return None;
    }
    let has_stop = input.stop.is_some();
    Some(Legs {
        tp,
        stop,
        slip: if has_stop { slip } else { 0 },
        band: if has_stop { band } else { 0 },
        keeper_tip: if has_stop { keeper_tip } else { 0 },
        min_touch,
        min_rest: MIN_REST_DAA,
        stop_worst,
        expected_updates: i64::from(has_stop),
    })
}

fn cond_ask_state(env: &Env, amount: i64, min_fill: i64, tip: i64, expiry: i64, active_from: i64, l: &Legs) -> CondAskState {
    let m = &env.market;
    CondAskState {
        maker: env.maker,
        token_cov_id: m.cov,
        token_tpl_hash: m.tpl_hash,
        tpl_prefix_len: m.prefix_len,
        tpl_suffix_len: m.suffix_len,
        scale: m.scale,
        min_fill,
        tip,
        active_from,
        expiry_daa: expiry,
        refund_tip: m.refund_tip,
        tp_price: l.tp,
        stop_price: l.stop,
        slip_bps: l.slip,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: l.min_touch,
        min_rest_daa: l.min_rest,
        armed: 0,
        band_daa: l.band,
        keeper_tip: l.keeper_tip,
        amount_left: amount,
        parent: [0; 32],
        rpt_price: 0,
        rpt_until: 0,
        extension_commitment: m.ext,
    }
}

#[allow(clippy::too_many_arguments)]
fn cond_bid_state(
    env: &Env,
    amount: i64,
    min_fill: i64,
    tip: i64,
    expiry: i64,
    active_from: i64,
    carrier: i64,
    l: &Legs,
) -> CondBidState {
    let m = &env.market;
    CondBidState {
        maker: env.maker,
        token_cov_id: m.cov,
        token_tpl_hash: m.tpl_hash,
        tpl_prefix_len: m.prefix_len,
        tpl_suffix_len: m.suffix_len,
        extension_commitment: m.ext,
        scale: m.scale,
        min_fill,
        tip,
        active_from,
        expiry_daa: expiry,
        refund_tip: m.refund_tip,
        delivery_carrier: carrier,
        tp_price: l.tp,
        stop_price: l.stop,
        slip_bps: l.slip,
        trail_step: 0,
        trail_gap: 0,
        trail_wait: 0,
        min_touch: l.min_touch,
        min_rest_daa: l.min_rest,
        amount_left: amount,
        armed: 0,
        band_daa: l.band,
        keeper_tip: l.keeper_tip,
        parent: [0; 32],
        rpt_price: 0,
        rpt_pre: 0,
        rpt_until: 0,
    }
}

fn bid_worst(l: &Legs) -> i64 {
    if l.stop > 0 && l.stop_worst > l.tp {
        l.stop_worst
    } else {
        l.tp
    }
}
fn ask_worst(l: &Legs) -> i64 {
    let mut c = vec![];
    if l.tp > 0 {
        c.push(l.tp);
    }
    if l.stop > 0 {
        c.push(l.stop_worst);
    }
    c.into_iter().min().unwrap_or(0)
}

struct Act {
    active_from: i64,
    activates_at: Option<i64>,
}

fn resolve_activation(env: &Env, log: &mut Vec<Issue>, a: Option<Activation>) -> Option<Act> {
    let Some(requested) = activation_daa(&env.clock, a) else { return Some(Act { active_from: 0, activates_at: None }) };
    let future = requested > env.clock.daa;
    let active_from = if future { requested } else { 0 };
    let bad: Vec<Issue> = check_times(&env.clock, active_from, env.clock.daa + 1, Some(requested))
        .into_iter()
        .filter(|i| matches!(i.code, "ACTIVE_IN_PAST" | "ACTIVE_TOO_FAR"))
        .collect();
    let stop = has_error(&bad);
    log.extend(bad);
    if stop {
        return None;
    }
    Some(Act { active_from, activates_at: future.then_some(requested) })
}

fn resolve_expiry(env: &Env, log: &mut Vec<Issue>, lt: Lifetime, active_from: i64) -> Option<Expiry> {
    let mut pre = vec![];
    let e = resolve_lifetime(&env.clock, lt, &mut pre);
    log.extend(pre);
    let bad: Vec<Issue> = check_times(&env.clock, active_from, e.daa, None)
        .into_iter()
        .filter(|i| matches!(i.code, "EXPIRY_TOO_FAR" | "EXPIRY_TOO_SOON" | "ACTIVE_AFTER_EXPIRY"))
        .collect();
    if !bad.is_empty() {
        log.extend(bad);
        return None;
    }
    Some(e)
}

fn min_fill_price(side: Side, l: &LegInput) -> i64 {
    let c: Vec<i64> = [l.tp, l.stop].into_iter().flatten().filter(|p| *p > 0).collect();
    match side {
        Side::Sell => c.into_iter().min().unwrap_or(0),
        Side::Buy => c.into_iter().max().unwrap_or(0),
    }
}

struct CondSpec {
    order: AnyState,
    exit: Option<AnyState>,
    lines: Vec<CarrierLine>,
    carrier: i64,
    side: Side,
    amount: i64,
    expiry: Expiry,
    activates_at: Option<i64>,
    limit_price: i64,
    all_in_price: i64,
    all_in_total: Option<i64>,
    expected_price: Option<i64>,
    worst_price: i64,
    tip: i64,
    min_touch: Option<i64>,
    notes: Vec<&'static str>,
}

fn finish_cond(env: &Env, mut log: Vec<Issue>, spec: CondSpec) -> Plan {
    let mut states = vec![spec.order.clone()];
    states.extend(spec.exit.clone());
    let value: i128 = spec.lines.iter().map(|l| l.amount * l.count as i128).sum();
    let holds = matches!(
        spec.order,
        AnyState::KobCondAsk(_) | AnyState::KobCondAskKron(_) | AnyState::KobIfdAsk(_) | AnyState::KobIfdAskKron(_)
    );
    let token_amount = if holds { spec.amount } else { 0 };
    let Ok(value) = i64::try_from(value) else {
        log.push(error("AMOUNT_TOO_LARGE", "the amount is too large for one order"));
        return Plan { states, ..failed(log) };
    };
    let env2 = Env { carrier: Some(spec.carrier as u64), ..env.clone() };
    let res = build_create(&env2, &CreateSpec { order: spec.order.clone(), value, token_amount, deadline: spec.expiry.deadline });
    log.extend(res.issues.clone());
    let Some(built) = res.built.clone() else { return Plan { states, ..failed(log) } };
    if has_error(&log) {
        return Plan { states, ..failed(log) };
    }
    let mut carriers = spec.lines.clone();
    if holds {
        carriers.push(CarrierLine { kind: "custody carrier", amount: res.token_carrier as i128, count: 1, kept: false });
        if res.token_change > 0 {
            carriers.push(CarrierLine { kind: "token change carrier", amount: res.token_carrier as i128, count: 1, kept: true });
        }
    }
    let kas_locked = carriers.iter().filter(|c| !c.kept).map(|c| c.amount * c.count as i128).sum();
    let (refund_tip, keeper_tip) = match &spec.order {
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => (s.refund_tip, s.keeper_tip),
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => (s.refund_tip, s.keeper_tip),
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => (s.refund_tip, s.keeper_tip),
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => (s.refund_tip, s.keeper_tip),
        _ => (0, 0),
    };
    let mut notes = spec.notes.clone();
    notes.push("carrierReturned");
    let disclosure = Disclosure {
        side: spec.side,
        amount: spec.amount,
        scale: env.market.scale,
        min_fill: spec.order.min_fill(),
        min_touch: spec.min_touch,
        limit_price: Some(spec.limit_price),
        all_in_price: Some(spec.all_in_price),
        all_in_total: spec.all_in_total,
        expected_price: spec.expected_price,
        worst_price: Some(spec.worst_price),
        tip: spec.tip,
        carriers,
        kas_locked,
        tokens_escrowed: token_amount,
        quote_escrowed: 0,
        fee: built.fee.fee,
        refund_tip,
        keeper_tip,
        expiry_kind: spec.expiry.kind,
        expiry_daa: spec.expiry.daa,
        expiry_unix: spec.expiry.deadline.unwrap_or_else(|| daa_to_unix(&env.clock, spec.expiry.daa)),
        deadline: spec.expiry.deadline,
        activates_at: spec.activates_at.map(|a| (a, daa_to_unix(&env.clock, a))),
        notes,
    };
    Plan { ok: true, issues: log, states, request: res.request, built: Some(built), disclosure: Some(disclosure) }
}

fn plan_cond(env: &Env, i: &CondIntent) -> Plan {
    let m = &env.market;
    let mut log = check_amount(i.amount, m, i.side == Side::Sell);
    if i.tip < 0 {
        log.push(error("TIP_NEGATIVE", "the priority tip cannot be negative"));
    }
    if has_error(&log) {
        return failed(log);
    }
    let carrier = env.carrier();
    let sell = i.side == Side::Sell;
    let input = match i.kind {
        CondKind::TakeProfit => LegInput { tp: i.take_profit, stop: None, limit: None, slip_bps: None, min_touch: None },
        CondKind::StopMarket => LegInput { tp: None, stop: i.stop, limit: None, slip_bps: i.slip_bps, min_touch: i.min_touch },
        CondKind::StopLimit => LegInput { tp: None, stop: i.stop, limit: i.limit, slip_bps: None, min_touch: i.min_touch },
        CondKind::Oco => LegInput { tp: i.take_profit, stop: i.stop, limit: i.limit, slip_bps: i.slip_bps, min_touch: i.min_touch },
    };
    let need = |x: Option<i64>, what: &str, log: &mut Vec<Issue>| {
        if x.is_none() {
            log.push(error("COND_LEGS_MISSING", format!("this order type needs {what}")));
        }
    };
    match i.kind {
        CondKind::TakeProfit => need(i.take_profit, "--take-profit", &mut log),
        CondKind::StopMarket => need(i.stop, "--stop", &mut log),
        CondKind::StopLimit => {
            need(i.stop, "--stop", &mut log);
            need(i.limit, "--limit", &mut log);
        }
        CondKind::Oco => {
            need(i.stop, "--stop", &mut log);
            need(i.take_profit, "--take-profit", &mut log);
        }
    }
    if has_error(&log) {
        return failed(log);
    }
    let min_fill = i.min_fill.unwrap_or_else(|| default_min_fill_for(m, i.amount, min_fill_price(i.side, &input)));
    log.extend(check_min_fill(min_fill, i.amount, "the minimum fill"));
    if has_error(&log) {
        return failed(log);
    }
    let legs = resolve_legs(env, &mut log, i.side, &input, "", min_fill, i.amount, m.keeper_tip);
    let act = resolve_activation(env, &mut log, i.active_from);
    let expiry = act.as_ref().and_then(|a| resolve_expiry(env, &mut log, i.lifetime, a.active_from));
    let (Some(legs), Some(act), Some(expiry)) = (legs, act, expiry) else { return failed(log) };
    let has_stop = legs.stop > 0;
    let has_tp = legs.tp > 0;
    let all_in = |p: i64| if sell { p - i.tip } else { p + i.tip };
    let worst_state = if sell { ask_worst(&legs) } else { bid_worst(&legs) };
    log.extend(check_tip(i.side, i.tip, worst_state));
    let top = legs.tp.max(legs.stop);
    log.extend(check_notional(i.amount, if sell { top } else { legs.stop_worst.max(top) + i.tip }, m.scale));
    if has_error(&log) {
        return failed(log);
    }
    log.extend(check_self_trade(&env.own, i.side, all_in(worst_state), 0));
    if let (true, Some(a)) = (has_stop && sell, env.book.asks.first()) {
        if a.price <= legs.stop {
            log.push(warning("COND_STOP_ALREADY_REACHED", "the market is already at or below the stop"));
        }
    }
    if let (true, Some(b)) = (has_stop && !sell, env.book.bids.first()) {
        if b.price >= legs.stop {
            log.push(warning("COND_STOP_ALREADY_REACHED", "the market is already at or above the stop"));
        }
    }
    if has_tp {
        if let Some(t) = crossing_touch(&env.book, i.side, legs.tp, i.tip) {
            log.push(warning("COND_TP_CROSSES", format!("the take-profit crosses the market ({t}): it can fill at once")));
        }
    }
    let keeper_reserve = legs.keeper_tip * legs.expected_updates;
    let mut lines = vec![];
    let order = if sell {
        let s = cond_ask_state(env, i.amount, min_fill, i.tip, expiry.daa, act.active_from, &legs);
        lines.push(CarrierLine { kind: "order carrier", amount: carrier as i128, count: 1, kept: false });
        if has_stop && keeper_reserve + m.refund_tip > carrier {
            log.push(error(
                "COND_KEEPER_FUNDING_TOO_LARGE",
                format!("the keeper reserve ({keeper_reserve} sompi) does not fit the carrier"),
            ));
            return failed(log);
        }
        match m.family {
            Family::Kcc20 => AnyState::KobCondAsk(s),
            Family::Kron => AnyState::KobCondAskKron(s),
        }
    } else {
        let max_fills = ceil_div(i.amount as i128, min_fill as i128) as i64;
        let fills = i.max_fills.unwrap_or(DEFAULT_BID_FILLS.min(max_fills));
        if fills < 1 || fills > max_fills {
            log.push(error("MAX_FILLS_INVALID", format!("the number of fills must be 1..{max_fills}")));
            return failed(log);
        }
        let s = cond_bid_state(env, i.amount, min_fill, i.tip, expiry.daa, act.active_from, carrier, &legs);
        let Some(escrow) = s.escrow(fills) else {
            log.push(error("AMOUNT_TOO_LARGE", "the amount is too large for one order"));
            return failed(log);
        };
        lines.push(CarrierLine { kind: "escrow", amount: escrow as i128 - carrier as i128 * fills as i128, count: 1, kept: false });
        lines.push(CarrierLine { kind: "delivery carrier", amount: carrier as i128, count: fills, kept: false });
        if has_stop && keeper_reserve > 0 {
            lines.push(CarrierLine {
                kind: "keeper reserve",
                amount: legs.keeper_tip as i128,
                count: legs.expected_updates,
                kept: false,
            });
        }
        match m.family {
            Family::Kcc20 => AnyState::KobCondBid(s),
            Family::Kron => AnyState::KobCondBidKron(s),
        }
    };
    let limit_price = if has_tp { legs.tp } else { legs.stop_worst };
    let expected = if has_tp { (!has_stop).then_some(legs.tp) } else { Some(legs.stop) };
    let worst = if has_tp && has_stop {
        if sell {
            legs.tp.min(legs.stop_worst)
        } else {
            legs.tp.max(legs.stop_worst)
        }
    } else {
        limit_price
    };
    let all_in_total = match &order {
        AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.proceeds(i.amount, limit_price),
        AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.spend(i.amount, limit_price),
        _ => None,
    };
    let mut notes: Vec<&'static str> = match i.kind {
        CondKind::StopMarket => vec!["stopTrigger", "stopAuction", "triggerExposure"],
        CondKind::StopLimit => vec!["stopTrigger", "stopAuction", "triggerExposure", "stopLimitMayNotFill"],
        CondKind::TakeProfit => vec!["takeProfitLeg"],
        CondKind::Oco => vec!["oco", "partialFillsKeepLegs", "stopTrigger", "stopAuction", "triggerExposure"],
    };
    if has_stop {
        notes.push("keeperReserve");
    }
    finish_cond(
        env,
        log,
        CondSpec {
            order,
            exit: None,
            lines,
            carrier,
            side: i.side,
            amount: i.amount,
            expiry,
            activates_at: act.activates_at,
            limit_price,
            all_in_price: all_in(limit_price),
            all_in_total,
            expected_price: expected,
            worst_price: worst,
            tip: i.tip,
            min_touch: has_stop.then_some(legs.min_touch),
            notes,
        },
    )
}

fn plan_ifd(env: &Env, i: &IfdIntent) -> Plan {
    let m = &env.market;
    let mut log = check_amount(i.amount, m, true);
    if i.tip < 0 || i.exit_tip < 0 {
        log.push(error("TIP_NEGATIVE", "the priority tip cannot be negative"));
    }
    if has_error(&log) {
        return failed(log);
    }
    let carrier = env.carrier();
    let buy_first = i.side == Side::Buy;
    let exit_side = i.side.other();
    if i.stop_limit.is_some() && i.stop.is_none() {
        log.push(error("COND_EXIT_LIMIT_NEEDS_STOP", "an exit stop-limit needs an exit stop"));
        return failed(log);
    }
    if i.take_profit.is_none() && i.stop.is_none() {
        log.push(error("COND_EXIT_NEEDS_ONE_LEG", "an if-done order needs an exit take-profit and / or an exit stop"));
        return failed(log);
    }
    log.extend(check_price(m, i.price, "the entry price"));
    if has_error(&log) {
        return failed(log);
    }
    let entry_all_in = if buy_first { i.price + i.tip } else { i.price - i.tip };
    if entry_all_in <= 0 {
        log.push(error("TIP_EXCEEDS_PRICE", "the priority tip must be smaller than the entry price"));
        return failed(log);
    }
    let min_fill = i.min_fill.unwrap_or_else(|| default_min_fill_ifd(i.amount));
    if min_fill < 1 || min_fill > i.amount {
        log.push(error("COND_MIN_FILL_INVALID", "the minimum fill must be 1..the amount"));
        return failed(log);
    }
    let fills = ceil_div(i.amount as i128, min_fill as i128) as i64;
    let exit_input = LegInput { tp: i.take_profit, stop: i.stop, limit: i.stop_limit, slip_bps: None, min_touch: None };
    let exit_min_default = default_min_fill_for(m, i.amount, min_fill_price(exit_side, &exit_input));
    let exit_min_fill = exit_min_default.min(min_fill);
    log.extend(check_min_fill(exit_min_fill, i.amount, "the exit minimum fill"));
    if has_error(&log) {
        return failed(log);
    }
    let exit_tip = i.exit_tip;
    let legs = resolve_legs(env, &mut log, exit_side, &exit_input, "exit ", exit_min_fill, i.amount, m.keeper_tip);
    let act = resolve_activation(env, &mut log, i.active_from);
    let entry_expiry = act.as_ref().and_then(|a| resolve_expiry(env, &mut log, i.lifetime, a.active_from));
    let (Some(legs), Some(act), Some(entry_expiry)) = (legs, act, entry_expiry) else { return failed(log) };
    if has_error(&log) {
        return failed(log);
    }
    // the exit's GTC is "90 days idle from its creation": committed as 2^62
    let exit_expiry = 1i64 << 62;
    if legs.tp > 0 {
        let tp_all_in = if buy_first { legs.tp - exit_tip } else { legs.tp + exit_tip };
        let profit = if buy_first { tp_all_in - entry_all_in } else { entry_all_in - tp_all_in };
        if profit <= 0 {
            log.push(warning("COND_TP_NOT_PROFITABLE", format!("the exit take-profit loses {} per token against the entry", -profit)));
        }
    }
    log.extend(check_notional(i.amount, if buy_first { i.price + i.tip } else { i.price }, m.scale));
    let exit_top = legs.stop_worst.max(legs.tp);
    log.extend(check_notional(i.amount, if buy_first { exit_top } else { exit_top + exit_tip }, m.scale));
    if has_error(&log) {
        return failed(log);
    }
    log.extend(check_self_trade(&env.own, i.side, entry_all_in, 0));
    let exit_worst_all_in = if buy_first { ask_worst(&legs) - exit_tip } else { bid_worst(&legs) + exit_tip };
    for mut w in check_self_trade(&env.own, exit_side, exit_worst_all_in, 0) {
        w.severity = Severity::Warning;
        log.push(w);
    }
    if let Some(t) = touch_price(&env.book, i.side) {
        if if buy_first { t <= i.price } else { t >= i.price } {
            log.push(warning("COND_ENTRY_CROSSES", "the entry crosses the book: it fills right away at its limit"));
        }
    }
    if has_error(&log) {
        return failed(log);
    }
    let mut lines = vec![];
    let fam = m.family;
    let (order, exit, prefund) = if buy_first {
        let ask = cond_ask_state(env, i.amount, exit_min_fill, exit_tip, exit_expiry, 0, &legs);
        let bid = IfdBidState {
            maker: env.maker,
            token_cov_id: m.cov,
            token_tpl_hash: m.tpl_hash,
            tpl_prefix_len: m.prefix_len,
            tpl_suffix_len: m.suffix_len,
            extension_commitment: m.ext,
            scale: m.scale,
            amount_left: i.amount,
            price: i.price,
            tip: i.tip,
            active_from: act.active_from,
            expiry_daa: entry_expiry.daa,
            refund_tip: m.refund_tip,
            delivery_carrier: carrier,
            exit_carrier: carrier,
            min_fill,
            entry_stop: 0,
            band_daa: 0,
            min_touch: 0,
            min_rest_daa: MIN_REST_DAA,
            keeper_tip: 0,
            armed: 0,
            rpt_amount: 0,
            exit_state: IfdBidState::commit_exit(&ask),
        };
        let Some(escrow) = bid.escrow() else {
            log.push(error("AMOUNT_TOO_LARGE", "the amount is too large for one order"));
            return failed(log);
        };
        lines.push(CarrierLine {
            kind: "escrow",
            amount: escrow as i128 - 2 * carrier as i128 * fills as i128,
            count: 1,
            kept: false,
        });
        lines.push(CarrierLine { kind: "delivery carrier", amount: carrier as i128, count: fills, kept: false });
        lines.push(CarrierLine { kind: "exit carrier", amount: carrier as i128, count: fills, kept: false });
        let (o, x) = match fam {
            Family::Kcc20 => (AnyState::KobIfdBid(bid), AnyState::KobCondAsk(ask)),
            Family::Kron => (AnyState::KobIfdBidKron(bid), AnyState::KobCondAskKron(ask)),
        };
        (o, x, None)
    } else {
        let bid_exit = cond_bid_state(env, i.amount, exit_min_fill, exit_tip, exit_expiry, 0, carrier, &legs);
        let proceeds = i.price - i.tip;
        let worst_buy_back = bid_worst(&legs) + exit_tip;
        let min_prefund = (worst_buy_back - proceeds).max(0);
        let prefund = i.prefund.unwrap_or(min_prefund);
        if prefund < min_prefund {
            log.push(error("COND_PREFUND_SHORT", format!("the prefund must be at least {min_prefund} per token (given {prefund})")));
            return failed(log);
        }
        let ask = IfdAskState {
            maker: env.maker,
            token_cov_id: m.cov,
            token_tpl_hash: m.tpl_hash,
            tpl_prefix_len: m.prefix_len,
            tpl_suffix_len: m.suffix_len,
            scale: m.scale,
            price: i.price,
            tip: i.tip,
            active_from: act.active_from,
            expiry_daa: entry_expiry.daa,
            refund_tip: m.refund_tip,
            prefund,
            exit_carrier: carrier,
            min_fill,
            entry_stop: 0,
            band_daa: 0,
            min_touch: 0,
            min_rest_daa: MIN_REST_DAA,
            keeper_tip: 0,
            armed: 0,
            amount_left: i.amount,
            rpt_amount: 0,
            exit_state: IfdAskState::commit_exit_for(fam, &bid_exit),
        };
        let Some(value) = ask.escrow(carrier) else {
            log.push(error("AMOUNT_TOO_LARGE", "the amount is too large for one order"));
            return failed(log);
        };
        lines.push(CarrierLine { kind: "order carrier", amount: carrier as i128, count: 1, kept: false });
        let prefund_kas = value as i128 - carrier as i128 - carrier as i128 * fills as i128;
        if prefund_kas > 0 {
            lines.push(CarrierLine { kind: "prefund", amount: prefund_kas, count: 1, kept: false });
        }
        lines.push(CarrierLine { kind: "exit carrier", amount: carrier as i128, count: fills, kept: false });
        let (o, x) = match fam {
            Family::Kcc20 => (AnyState::KobIfdAsk(ask), AnyState::KobCondBid(bid_exit)),
            Family::Kron => (AnyState::KobIfdAskKron(ask), AnyState::KobCondBidKron(bid_exit)),
        };
        (o, x, Some(prefund))
    };
    if legs.keeper_tip * legs.expected_updates * 2 > carrier {
        log.push(error("COND_KEEPER_FUNDING_TOO_LARGE", "the exit keeper reserve does not fit the carrier"));
        return failed(log);
    }
    let all_in_total = match &order {
        AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.spend(i.amount, i.price),
        AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.proceeds(i.amount, i.price),
        _ => None,
    };
    let mut notes: Vec<&'static str> = vec![if i.take_profit.is_some() && i.stop.is_some() { "ifo" } else { "ifd" }, "position"];
    notes.extend([if buy_first { "buyFirst" } else { "sellFirst" }, "minFill", "exitGtc"]);
    if legs.stop > 0 {
        notes.extend(["exitStop", "triggerExposure"]);
    }
    if prefund.is_some() {
        notes.push("prefund");
    }
    finish_cond(
        env,
        log,
        CondSpec {
            order,
            exit: Some(exit),
            lines,
            carrier,
            side: i.side,
            amount: i.amount,
            expiry: entry_expiry,
            activates_at: act.activates_at,
            limit_price: i.price,
            all_in_price: entry_all_in,
            all_in_total,
            expected_price: None,
            worst_price: i.price,
            tip: i.tip,
            min_touch: None,
            notes,
        },
    )
}

// ------------------------------------------------------------------------------------------------ pair limit (pair-*.ts)

fn plan_pair_limit(env: &Env, i: &LimitIntent) -> Plan {
    let Some(pair) = &env.pair else { return failed(vec![error("PAIR_MISSING", "a pair order needs a quote token")]) };
    let a = &env.market;
    let b = &pair.quote;
    if a.cov == b.cov {
        return failed(vec![error("PAIR_SAME_TOKEN", "a pair needs two different tokens")]);
    }
    let mut pre = vec![];
    if i.price <= 0 {
        pre.push(error("PRICE_NOT_POSITIVE", "the price must be greater than zero"));
    }
    if i.max_fills.is_some_and(|f| f < 1) {
        pre.push(error("MAX_FILLS_INVALID", "the number of fills must be a positive integer"));
    }
    if has_error(&pre) {
        return failed(pre);
    }
    let mut d = blank(i.side, i.amount, i.price, i.tip, i.min_fill);
    d.fills = None;
    let requested = activation_daa(&env.clock, i.active_from);
    let future = requested.is_some_and(|r| r > env.clock.daa);
    d.requested_active_from = requested;
    d.active_from = if future { requested.unwrap_or(0) } else { 0 };
    d.activates_at = if future { requested } else { None };
    let exp = resolve_lifetime(&env.clock, i.lifetime, &mut pre);
    d.expiry_daa = exp.daa;
    d.deadline = exp.deadline;
    d.expiry_kind = exp.kind;
    d.fills_requested = i.max_fills;
    d.worst_price = Some(i.price);
    d.notes.push(match exp.kind {
        "gtc" => "gtc",
        "day" => "dayOrder",
        _ => "gtd",
    });
    pre.extend(check_price_band(reference_price(&env.book), i.side, i.price));
    let touch = if future { None } else { crossing_touch(&env.book, i.side, i.price, 0) };
    if let Some(t) = touch {
        let start = if i.side == Side::Sell { t.max(i.price) } else { t.min(i.price) };
        match i.crossing {
            Crossing::Reject => pre.push(error("PAIR_MARKETABLE_REJECTED", format!("your limit crosses the pair book at {t}"))),
            Crossing::Auction if start != i.price => {
                d.price = start;
                d.price_end = i.price;
                d.slope = decay_slope(start, i.price, MARKET_AUCTION_DAA, 1);
                d.decay_step = 1;
                d.active_from = env.clock.daa + MARKET_ACTIVATION_DAA;
                d.activates_at = None;
                d.reach = i.price;
                d.expected_price = Some(t);
                d.notes.extend(["auction", "marketable"]);
                pre.push(info(
                    "PAIR_MARKETABLE_AUCTION",
                    format!("your limit crosses the pair book: placed as an auction from {t} to your limit"),
                ));
            }
            _ if start != i.price => pre.push(warning(
                "PAIR_MARKETABLE_LIMIT_FILLS_AT_LIMIT",
                format!("your limit crosses the pair book ({t}): it fills at your limit"),
            )),
            _ => {}
        }
    }
    place_pair(env, pair, d, pre)
}

fn place_pair(env: &Env, pair: &PairPart, d: Draft, mut issues: Vec<Issue>) -> Plan {
    let a = &env.market;
    let b = &pair.quote;
    issues.extend(check_amount(d.amount, a, d.side == Side::Sell));
    if d.tip < 0 {
        issues.push(error("TIP_NEGATIVE", "the priority tip cannot be negative"));
    } else if ceil_div(d.amount as i128 * d.tip as i128, a.scale as i128) >= QUOTE_LIMIT {
        issues.push(error("TIP_TOO_LARGE", "the priority tip is too large for this quantity"));
    }
    issues.extend(check_times(&env.clock, d.active_from, d.expiry_daa, d.requested_active_from));
    if has_error(&issues) {
        return failed(issues);
    }
    let mut min_fill =
        d.min_fill.unwrap_or_else(|| if d.immediate { 1 } else { default_min_fill_pair(d.amount, pair.kas_per_whole_a, a.scale) });
    if d.min_fill.is_none() && !d.immediate && pair.kas_per_whole_a.is_none() {
        issues.push(warning(
            "PAIR_KAS_REFERENCE_MISSING",
            format!("no KAS price of {} is known: the default minimum fill is a quarter of the amount", a.name()),
        ));
    }
    if d.min_fill.is_none() && d.max_fill > 0 && min_fill > d.max_fill {
        min_fill = d.max_fill;
    }
    issues.extend(check_min_fill(min_fill, d.amount, "the minimum fill"));
    let top = d.price.max(d.price_end);
    if ceil_div(d.amount as i128 * top as i128, a.scale as i128) >= QUOTE_LIMIT {
        issues.push(error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large"));
    }
    if has_error(&issues) {
        return failed(issues);
    }
    issues.extend(check_self_trade(&env.own, d.side, d.reach, 0));
    if has_error(&issues) {
        return failed(issues);
    }
    let possible = ceil_div(d.amount as i128, min_fill as i128) as i64;
    let fills = d.fills.or(d.fills_requested).unwrap_or(possible.min(MAX_DEFAULT_PAIR_FILLS));
    if d.tif == 0 && d.interval == 0 && fills < possible {
        issues.push(info(
            "PAIR_FILLS_LIMITED",
            format!("{fills} delivery carriers: after {} partial fills what is left fills only in full", fills - 1),
        ));
    }
    let dc = env.carrier();
    let tips = match pair_tips(a.program, b.program) {
        Ok(t) => t,
        Err(e) => return failed(vec![error("BUILD_REJECTED", e.to_string())]),
    };
    let (s, t) = if d.side == Side::Sell { (a, b) } else { (b, a) };
    let fam = |m: &TokenMarket| if m.family == Family::Kron { 2 } else { 1 };
    let ext = |m: &TokenMarket| if m.family == Family::Kron { [0; 32] } else { m.ext };
    let mut st = PairState {
        maker: env.maker,
        side: if d.side == Side::Sell { SIDE_ASK } else { SIDE_BID },
        s_cov_id: s.cov,
        s_tpl_hash: s.tpl_hash,
        s_pre: s.prefix_len,
        s_suf: s.suffix_len,
        s_family: fam(s),
        s_scale: s.scale,
        t_cov_id: t.cov,
        t_tpl_hash: t.tpl_hash,
        t_pre: t.prefix_len,
        t_suf: t.suffix_len,
        t_family: fam(t),
        t_ext: ext(t),
        t_scale: t.scale,
        min_fill,
        price: d.price,
        tip: d.tip,
        tif: d.tif,
        active_from: d.active_from,
        expiry_daa: d.expiry_daa,
        refund_tip: tips.refund_tip as i64,
        delivery_carrier: dc,
        interval: d.interval,
        max_fill: d.max_fill,
        slope: d.slope,
        price_end: d.price_end,
        decay_step: d.decay_step,
        amount_left: d.amount,
        custody: d.amount,
        s_ext: ext(s),
    };
    if d.side == Side::Buy {
        let f = if d.tif == 0 { st.max_fills() } else { 1 };
        let Some(escrow) = st.bid_escrow(d.amount, f) else {
            issues.push(error("PAIR_NOTIONAL_TOO_LARGE", "the order value in the quote token is too large"));
            return failed(issues);
        };
        st.custody = escrow;
    }
    let funded = fills.max(st.funded_fills());
    let Some(total) = st.kas_value(funded) else {
        issues.push(error("TIP_TOO_LARGE", "the priority tip is too large for this quantity"));
        return failed(issues);
    };
    let tip_total = total - dc * funded.max(1);
    let mut lines = vec![CarrierLine { kind: "delivery carrier", amount: dc as i128, count: funded.max(1), kept: false }];
    if tip_total > 0 {
        lines.push(CarrierLine { kind: "tip prefund", amount: tip_total as i128, count: 1, kept: false });
    }
    let order = AnyState::KobPair(st.clone());
    // KRON caps of what the order holds and delivers
    for (tok, amount) in order.custodies() {
        let m = if tok == a.cov { a } else { b };
        if m.family == Family::Kron && amount > KRON_MAX_OUTPUT_AMOUNT {
            issues.push(error(
                "PAIR_KRON_CUSTODY_TOO_LARGE",
                format!("a KRON custody of {} holds at most {KRON_MAX_OUTPUT_AMOUNT} base units", m.name()),
            ));
        }
    }
    let n1 = min_fill.min(d.amount);
    let delivery = if d.side == Side::Sell { (b, st.t_out_min(n1, st.price_max()).unwrap_or(0)) } else { (a, n1) };
    if delivery.0.family == Family::Kron && delivery.1 > KRON_MAX_OUTPUT_AMOUNT {
        issues.push(error("PAIR_KRON_DELIVERY_TOO_LARGE", "a minimum fill would deliver more than a KRON output holds"));
    }
    if has_error(&issues) {
        return Plan { states: vec![order], ..failed(issues) };
    }
    if let Err(e) = check_new_order(&order) {
        issues.push(error("BUILD_REJECTED", format!("the protocol refused this order: {e}")));
        return Plan { states: vec![order], ..failed(issues) };
    }
    let value: i128 = lines.iter().map(|l| l.amount * l.count as i128).sum();
    let min_value = min_order_value(&order) as i128;
    if value < min_value || !fits(value) {
        issues.push(error("BUILD_REJECTED", format!("the order UTXO needs at least {min_value} sompi (planned {value})")));
        return Plan { states: vec![order], ..failed(issues) };
    }
    let value = value as i64;
    // the custodies: each from the maker's UTXOs of its own token
    let mut tokens = vec![];
    let mut draws: Vec<([u8; 32], i64)> = vec![];
    let mut outputs_kas = 0i128;
    let mut inputs_kas = 0i128;
    let mut changes = vec![];
    for (tok, amount) in order.custodies() {
        let (m, utxos) = if tok == a.cov { (a, &env.token_utxos) } else { (b, &pair.quote_utxos) };
        let floor = order_carrier_floor(m);
        if dc < floor {
            issues.push(error(
                "CARRIER_BELOW_FLOOR",
                format!("the carrier ({dc} sompi) is below {floor} sompi, the least a token output of {} carries", m.name()),
            ));
            return Plan { states: vec![order], ..failed(issues) };
        }
        let usable = usable_token_utxos(m, &env.maker, utxos);
        let picked = match select_tokens(&usable, amount, m.slots.0.min(MAX_TOKEN_INPUTS), &m.name()) {
            Ok(p) => p,
            Err(e) => {
                issues.push(e);
                return Plan { states: vec![order], ..failed(issues) };
            }
        };
        let have: i128 = picked.iter().map(|u| u.state.amount() as i128).sum();
        inputs_kas += picked.iter().map(|u| u.utxo.amount as i128).sum::<i128>();
        outputs_kas += dc as i128 + if have > amount as i128 { dc as i128 } else { 0 };
        changes.push((tok, have - amount as i128));
        tokens.extend(picked);
        draws.push((tok, amount));
    }
    let need = (value as i128 + outputs_kas - inputs_kas).max(0);
    let request = |funding: Vec<KeyUtxo>| CreateOrder {
        order: order.clone(),
        value: value as u64,
        tokens: tokens.clone(),
        token_carrier: if tokens.is_empty() { 0 } else { dc as u64 },
        funding,
        change: None,
        lock_time: 0,
        deadline: d.deadline.map(|x| x as u64),
        records: vec![],
        fee: fee_options(env),
    };
    let (req, built) = match build_with_reserve(env, need, request, |bt| verify_built(bt, &order, value, d.deadline, &draws)) {
        Ok(x) => x,
        Err(e) => {
            issues.push(e);
            return Plan { states: vec![order], ..failed(issues) };
        }
    };
    let mut carriers = lines.clone();
    let mut escrow_a = 0;
    let mut escrow_b = 0;
    for (k, (tok, amount)) in draws.iter().enumerate() {
        let is_a = *tok == a.cov;
        carriers.push(CarrierLine {
            kind: if is_a { "custody carrier" } else { "quote custody carrier" },
            amount: dc as i128,
            count: 1,
            kept: false,
        });
        if changes[k].1 > 0 {
            carriers.push(CarrierLine {
                kind: if is_a { "token change carrier" } else { "quote change carrier" },
                amount: dc as i128,
                count: 1,
                kept: true,
            });
        }
        if is_a {
            escrow_a += amount;
        } else {
            escrow_b += amount;
        }
    }
    let kas_locked = carriers.iter().filter(|c| !c.kept).map(|c| c.amount * c.count as i128).sum();
    let sell = d.side == Side::Sell;
    let b_of = |n: i64, p: i64| if sell { st.t_out_min(n, p) } else { st.s_out(n, p) };
    let disclosure = Disclosure {
        side: d.side,
        amount: d.amount,
        scale: a.scale,
        min_fill,
        min_touch: None,
        limit_price: Some(d.limit_price),
        all_in_price: Some(d.limit_price),
        all_in_total: b_of(d.amount, d.limit_price),
        expected_price: d.expected_price,
        worst_price: d.worst_price,
        tip: d.tip,
        carriers,
        kas_locked,
        tokens_escrowed: escrow_a,
        quote_escrowed: escrow_b,
        fee: built.fee.fee,
        refund_tip: st.refund_tip,
        keeper_tip: 0,
        expiry_kind: d.expiry_kind,
        expiry_daa: d.expiry_daa,
        expiry_unix: daa_to_unix(&env.clock, d.expiry_daa),
        deadline: d.deadline,
        activates_at: d.activates_at.filter(|x| *x > env.clock.daa).map(|x| (x, daa_to_unix(&env.clock, x))),
        notes: [d.notes.clone(), vec!["pairTipKas", "carrierReturned"]].concat(),
    };
    Plan { ok: !has_error(&issues), issues, states: vec![order], request: Some(req), built: Some(built), disclosure: Some(disclosure) }
}

// ------------------------------------------------------------------------------------------------ entry point (plan.ts)

fn env_issues(env: &Env) -> Vec<Issue> {
    let mut out = vec![];
    if !env.guards_unavailable.is_empty() {
        let g = env.guards_unavailable.join(", ");
        out.push(if env.guards_acknowledged {
            warning("GUARDS_UNAVAILABLE_ACKNOWLEDGED", format!("placed without the safety checks that need the indexer ({g})"))
        } else {
            error(
                "GUARDS_UNAVAILABLE",
                format!("the safety checks that need the indexer ({g}) could not run; --ack-no-guards places the order anyway"),
            )
        });
    }
    if let (Some(r), Some(l)) = (reference_price(&env.book).filter(|r| *r > 0), env.last_fill.filter(|l| *l > 0)) {
        let bps = (r as i128 - l as i128).abs() * 10_000 / l as i128;
        if bps >= 5_000 {
            out.push(warning(
                "MARKET_REFERENCE_DIVERGES",
                format!("the order book shows {r} per token but the last fill was at {l}: check the price yourself before signing"),
            ));
        }
    }
    out
}

fn market_start_issues(env: &Env, plan: &Plan) -> Vec<Issue> {
    let Some(d) = &plan.disclosure else { return vec![] };
    let Some(start) = d.expected_price.filter(|s| *s > 0) else { return vec![] };
    let buy = d.side == Side::Buy;
    let dir = if buy { "above" } else { "below" };
    let ack = env.market_start_ack;
    if let Some(r) = env.last_fill.filter(|r| *r > 0) {
        let gap = if buy { start as i128 - r as i128 } else { r as i128 - start as i128 };
        if gap > 0 {
            let bps = gap * 10_000 / r as i128;
            if bps > MARKET_START_TOLERANCE_BPS {
                return vec![if ack {
                    warning(
                        "MARKET_START_ACKNOWLEDGED",
                        format!("acknowledged: the auction start ({start}) is {bps} bps {dir} the last fill ({r})"),
                    )
                } else {
                    error(
                        "MARKET_START_VS_LAST_FILL",
                        format!(
                            "the auction starts at {start} per token, {bps} bps {dir} the last fill ({r}); the start is the indexer's best price and cannot be checked against the node: check it, then --ack-market-start"
                        ),
                    )
                }];
            }
        }
    }
    let value = d.amount as i128 * start as i128 / d.scale as i128;
    if value >= MARKET_START_UNVERIFIED_MIN_SOMPI {
        vec![if ack {
            warning(
                "MARKET_START_UNVERIFIED_ACKNOWLEDGED",
                format!("acknowledged: nothing independent of the indexer checks the auction start ({start})"),
            )
        } else {
            error(
                "MARKET_START_UNVERIFIED",
                format!(
                    "the auction starts at {start} per token, the indexer's best price, and nothing independent of that indexer checks it; check it, then --ack-market-start"
                ),
            )
        }]
    } else if env.last_fill.is_some() {
        vec![warning(
            "MARKET_START_NO_INDEPENDENT_REFERENCE",
            format!("the auction start ({start}) was compared only with the last fill of the same indexer"),
        )]
    } else {
        vec![warning(
            "MARKET_START_UNCHECKED",
            format!("nothing apart from the indexer book was available to compare the auction start ({start}) with"),
        )]
    }
}

fn stop_evidence_issues(env: &Env, plan: &Plan) -> Vec<Issue> {
    let has_stop = plan.states.iter().any(|s| match s {
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.stop_price > 0,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.stop_price > 0,
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => x.entry_stop > 0,
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => x.entry_stop > 0,
        _ => false,
    });
    if !has_stop || env.market.family != Family::Kcc20 || env.market.genesis_verified {
        return vec![];
    }
    vec![warning(
        "STOP_EVIDENCE_GENESIS_UNVERIFIED",
        format!(
            "the genesis of {} was not verified: trades of a look-alike token under the same covenant id can arm or trail this stop",
            env.market.name()
        ),
    )]
}

/// Plans an order. Never panics for user input: problems are issues (`ok: false` when one is an error).
pub fn plan_order(env: &Env, intent: &Intent) -> Plan {
    let mut plan = match intent {
        Intent::Limit(i) => plan_limit(env, i),
        Intent::Immediate(i) => plan_immediate(env, i),
        Intent::Market(i) => plan_market(env, i),
        Intent::Cond(i) => plan_cond(env, i),
        Intent::Ifd(i) => plan_ifd(env, i),
        Intent::PairLimit(i) => plan_pair_limit(env, i),
    };
    let mut extra = env_issues(env);
    if matches!(intent, Intent::Market(_)) {
        extra.extend(market_start_issues(env, &plan));
    }
    extra.extend(stop_evidence_issues(env, &plan));
    if has_error(&extra) {
        plan.ok = false;
    }
    plan.issues.extend(extra);
    plan.ok = plan.ok && plan.built.is_some() && !has_error(&plan.issues);
    plan
}
