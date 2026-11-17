//! How a buy-side order delivers what it buys (bid-delivery-spec §10, `web/src/kob/orders/delivery.ts`).
//!
//! `--delivery each|collected|one-utxo` is mode 0, 1 or 2. Each fill is the
//! default (founder decision 2026-11-17); collected and one-utxo are opt-in. IOC / FOK / market orders, KRON deliveries and a repeating entry's exits deliver each fill.

use kob_protocol::artifacts::TemplateId;
use kob_protocol::json::to_hex;
use kob_protocol::state::{MODE_ACCUMULATE, MODE_NEW, MODE_TOPUP};
use serde_json::{json, Value};

use super::plan::{error, warning, Issue};
use super::units;

/// The KAS a receive UTXO's threshold is sized to: 5 KAS of the token at the order's price.
pub const RECV_THRESHOLD_SOMPI: i128 = 500_000_000;
/// Most later fills a collected pair tip pays the custody input for.
pub const MAX_COLLECTED_LATER_FILLS: i64 = 63;
/// The extra relay fee of a collected pair fill after the first, sompi (B3's reference value on
/// `KCC20Ref_8x8`: 5,782,600 against 4,500,000), until a table has one per program pair.
pub const PAIR_COLLECTED_EXTRA: i64 = 1_282_600;

/// The three deliveries by the names of the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Mode 0: a new token UTXO for every fill.
    Each,
    /// Mode 1: the order keeps what it buys until it ends or is claimed.
    Collected,
    /// Mode 2: a fill may top up the maker's receive UTXO and keeps the tip.
    OneUtxo,
}

impl Choice {
    /// The choice named by `--delivery`.
    pub fn parse(s: &str) -> Result<Choice, String> {
        match s {
            "each" => Ok(Choice::Each),
            "collected" => Ok(Choice::Collected),
            "one-utxo" => Ok(Choice::OneUtxo),
            o => Err(format!("--delivery must be each, collected or one-utxo, got `{o}`")),
        }
    }
    /// The name of the command line.
    pub fn name(self) -> &'static str {
        match self {
            Choice::Each => "each",
            Choice::Collected => "collected",
            Choice::OneUtxo => "one-utxo",
        }
    }
    /// The state's `mode`.
    pub fn mode(self) -> i64 {
        match self {
            Choice::Each => MODE_NEW,
            Choice::Collected => MODE_ACCUMULATE,
            Choice::OneUtxo => MODE_TOPUP,
        }
    }
}

/// What the command line and the machine ask for. The default is no choice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// `--delivery`.
    pub choice: Option<Choice>,
    /// `--topup-tip`, sompi per fill.
    pub tip: Option<i64>,
    /// The threshold T stored for this (key, token, extension commitment), base units.
    pub threshold: Option<i64>,
    /// `--reset-threshold`: compute T again at this order's price and store it.
    pub reset: bool,
}

/// When the default rule applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A GTC order.
    Resting,
    /// IOC, FOK and market orders.
    Immediate,
    /// The exits of a repeating entry, which merge back into it.
    Repeating,
}

/// The order a delivery is chosen for.
#[derive(Clone, Copy, Debug)]
pub struct Order {
    /// The bought token is KRON.
    pub kron: bool,
    /// The bought token's program.
    pub program: TemplateId,
    /// Resting or not.
    pub class: Class,
    /// The order's delivery carrier, sompi.
    pub carrier: i64,
    /// Base units per whole bought token.
    pub scale: i64,
    /// The minimum fill, base units.
    pub min_fill: i64,
    /// The limit price in sompi per whole token; `None` for a pair order.
    pub price: Option<i64>,
    /// A pair order: one-utxo is not offered.
    pub pair: bool,
}

/// The delivery an order is placed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// What the order does.
    pub choice: Choice,
    /// `recvThreshold` of mode 2, else 0.
    pub recv_threshold: i64,
    /// `topupTip` of mode 2, else 0.
    pub topup_tip: i64,
    /// Mode 2: the wallet threshold T this order used.
    pub threshold: Option<i64>,
    /// T is new on this machine (or reset): keep it after the order is submitted.
    pub threshold_new: bool,
}

impl Resolved {
    /// Mode 0.
    pub const EACH: Resolved =
        Resolved { choice: Choice::Each, recv_threshold: 0, topup_tip: 0, threshold: None, threshold_new: false };
    /// The state's `mode`.
    pub fn mode(&self) -> i64 {
        self.choice.mode()
    }
    /// The delivery carriers a bid is funded with: one for collected (the custody rides on its
    /// first), else one per possible fill.
    pub fn funded(&self, fills: i64) -> i64 {
        if self.choice == Choice::Collected {
            1
        } else {
            fills
        }
    }
}

/// The request that keeps the delivery of `s` (a replacement keeps its order's mode).
pub fn request_of(s: &kob_protocol::state::AnyState) -> Request {
    let Some(d) = s.delivery().or_else(|| s.exit_delivery()) else { return Request::default() };
    let choice = match d.mode {
        MODE_ACCUMULATE => Choice::Collected,
        MODE_TOPUP => Choice::OneUtxo,
        _ => Choice::Each,
    };
    let topup = d.mode == MODE_TOPUP;
    Request { choice: Some(choice), tip: topup.then_some(d.topup_tip), threshold: topup.then_some(d.recv_threshold), reset: false }
}

/// `min(carrier, ceil(1.25 x topup_extra))` of the program, `None` without a measured top-up.
pub fn default_tip(program: TemplateId, carrier: i64) -> Option<i64> {
    kob_protocol::budget::default_topup_tip(program, carrier)
}

/// `T = max(1, ceil(5 KAS x scale / price))`.
pub fn wallet_threshold(price: i64, scale: i64) -> Option<i64> {
    if price <= 0 || scale <= 0 {
        return None;
    }
    let t = (RECV_THRESHOLD_SOMPI * scale as i128 + price as i128 - 1) / price as i128;
    i64::try_from(t.max(1)).ok()
}

/// The `recvThreshold` of an order: `min(T, minFill - 1)`, 0 when that is not positive.
pub fn order_threshold(t: i64, min_fill: i64) -> i64 {
    t.min(min_fill - 1).max(0)
}

/// What a collected pair order adds to its default tip, sompi per whole A:
/// `ceil(ceil(1.25 x extra) x L x scale / amount)`, `L = min(ceil(amount / minFill) - 1, 63)`.
pub fn collected_pair_tip_extra(scale: i64, amount: i64, min_fill: i64) -> i64 {
    if amount <= 0 || min_fill <= 0 {
        return 0;
    }
    let later = ((amount as i128 + min_fill as i128 - 1) / min_fill as i128 - 1).min(MAX_COLLECTED_LATER_FILLS as i128);
    if later <= 0 {
        return 0;
    }
    let per_fill = (PAIR_COLLECTED_EXTRA as i128 * 5 + 3) / 4;
    let v = (per_fill * later * scale as i128 + amount as i128 - 1) / amount as i128;
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The delivery of `o` from `r` (bid-delivery-spec §10); problems go to `issues`.
pub fn resolve(o: &Order, r: &Request, issues: &mut Vec<Issue>) -> Resolved {
    if o.kron || o.class != Class::Resting {
        if r.choice.is_some_and(|c| c != Choice::Each) {
            let why = if o.kron { "a KRON delivery" } else { "an IOC / FOK / market order or a repeating entry's exit" };
            issues.push(warning(
                "DELIVERY_EACH_FILL",
                format!("{why} delivers at each fill: --delivery {} is not applied", r.choice.map_or("", Choice::name)),
            ));
        }
        return Resolved::EACH;
    }
    match r.choice.unwrap_or(Choice::Each) {
        Choice::Each => Resolved::EACH,
        Choice::Collected => Resolved { choice: Choice::Collected, ..Resolved::EACH },
        Choice::OneUtxo => {
            let refuse = |issues: &mut Vec<Issue>, why: String| {
                issues.push(error("DELIVERY_ONE_UTXO_UNAVAILABLE", format!("--delivery one-utxo is not offered: {why}")));
                Resolved::EACH
            };
            if o.pair {
                return refuse(issues, "a pair order delivers each fill or collected".into());
            }
            let Some(default) = default_tip(o.program, o.carrier) else {
                return refuse(issues, format!("{} has no measured top-up", o.program.name()));
            };
            let stored = if r.reset { None } else { r.threshold.filter(|t| *t > 0) };
            let Some(t) = stored.or_else(|| o.price.and_then(|p| wallet_threshold(p, o.scale))) else {
                return refuse(issues, "no price to size the receive threshold".into());
            };
            let recv = order_threshold(t, o.min_fill);
            if recv == 0 {
                return refuse(
                    issues,
                    "the minimum fill is 1 base unit, so no fill can grow a receive UTXO past its threshold".into(),
                );
            }
            let tip = r.tip.unwrap_or(default);
            if tip < 0 || tip > o.carrier {
                issues.push(error(
                    "TOPUP_TIP_INVALID",
                    format!("--topup-tip must be 0 to the delivery carrier ({})", units::kas(o.carrier as i128)),
                ));
            } else if tip < default {
                issues.push(warning(
                    "TOPUP_TIP_LOW",
                    format!(
                        "a tip below {} may not pay a filler to top up: the fill then arrives as a new UTXO",
                        units::kas(default as i128)
                    ),
                ));
            }
            Resolved {
                choice: Choice::OneUtxo,
                recv_threshold: recv,
                topup_tip: tip,
                threshold: Some(t),
                threshold_new: stored.is_none(),
            }
        }
    }
}

/// How an order delivers, for its plan and view, `None` for a kind that delivers no tokens.
///
/// An if-done entry shows the delivery of its exits.
pub fn text(s: &kob_protocol::state::AnyState, unit: &super::units::TokenUnit) -> Option<String> {
    let exits = s.delivery().is_none();
    let d = s.delivery().or_else(|| s.exit_delivery())?;
    let of = if exits { "exits: " } else { "" };
    Some(match d.mode {
        MODE_ACCUMULATE if d.bought > 0 => {
            format!("{of}collected, {} bought and held by the order (kob claim)", units::tokens(d.bought as i128, unit))
        }
        MODE_ACCUMULATE => format!("{of}collected (the order keeps what it buys until it ends or you claim it)"),
        MODE_TOPUP => format!(
            "delivery: {of}one-utxo (top-up of your receive UTXO from {}, tip {} per fill)",
            units::tokens(d.recv_threshold as i128, unit),
            units::kas(d.topup_tip as i128)
        ),
        _ => format!("{of}each fill (a new UTXO per fill)"),
    })
}

// ------------------------------------------------------------------------- the stored threshold

const THRESHOLD_FILE: &str = "receive-thresholds.json";

fn threshold_key(network: &str, owner: &[u8; 32], token: &[u8; 32], ext: &[u8; 32]) -> String {
    format!("{network}:{}:{}:{}", to_hex(owner), to_hex(token), to_hex(ext))
}

/// The threshold T this machine stored for (network, key, token, extension commitment).
pub fn stored_threshold(network: &str, owner: &[u8; 32], token: &[u8; 32], ext: &[u8; 32]) -> Option<i64> {
    let path = crate::recent::state_dir()?.join(THRESHOLD_FILE);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get(threshold_key(network, owner, token, ext))?.as_str()?.parse().ok().filter(|t: &i64| *t > 0)
}

/// Keeps T after a submitted order. It never fails a command.
pub fn store_threshold(network: &str, owner: &[u8; 32], token: &[u8; 32], ext: &[u8; 32], t: i64) {
    let Some(dir) = crate::recent::state_dir() else { return };
    let path = dir.join(THRESHOLD_FILE);
    let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}));
    if !v.is_object() {
        v = json!({});
    }
    v[threshold_key(network, owner, token, ext)] = json!(t.to_string());
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{THRESHOLD_FILE}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&v).unwrap_or_default())?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        eprintln!("warning: cannot keep the receive threshold in {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: TemplateId = TemplateId::Kcc20Ref;

    fn order(class: Class) -> Order {
        Order {
            kron: false,
            program: P,
            class,
            carrier: 100_000_000,
            scale: 1000,
            min_fill: 5000,
            price: Some(250_000_000),
            pair: false,
        }
    }

    #[test]
    fn defaults_follow_the_class() {
        let mut v = vec![];
        assert_eq!(resolve(&order(Class::Resting), &Request::default(), &mut v), Resolved::EACH);
        let col = Request { choice: Some(Choice::Collected), ..Default::default() };
        assert_eq!(resolve(&order(Class::Resting), &col, &mut v).choice, Choice::Collected);
        for c in [Class::Immediate, Class::Repeating] {
            assert_eq!(resolve(&order(c), &Request::default(), &mut v), Resolved::EACH);
        }
        assert_eq!(resolve(&Order { kron: true, ..order(Class::Resting) }, &Request::default(), &mut v), Resolved::EACH);
        assert!(v.is_empty());
        // A choice that cannot apply is said, not obeyed.
        let ask = Request { choice: Some(Choice::Collected), ..Default::default() };
        assert_eq!(resolve(&order(Class::Immediate), &ask, &mut v), Resolved::EACH);
        assert_eq!(v[0].code, "DELIVERY_EACH_FILL");
    }

    #[test]
    fn one_utxo_threshold_and_tip() {
        // T = ceil(5 KAS x 1000 / 2.5 KAS) = 2000 base units, under minFill - 1.
        assert_eq!(wallet_threshold(250_000_000, 1000), Some(2000));
        assert_eq!(wallet_threshold(0, 1000), None);
        assert_eq!(order_threshold(2000, 5000), 2000);
        assert_eq!(order_threshold(9000, 5000), 4999);
        assert_eq!(order_threshold(9000, 1), 0);
        let mut v = vec![];
        let r = Request { choice: Some(Choice::OneUtxo), ..Default::default() };
        let d = resolve(&order(Class::Resting), &r, &mut v);
        assert!(v.is_empty(), "{v:?}");
        assert_eq!((d.choice, d.recv_threshold, d.threshold, d.threshold_new), (Choice::OneUtxo, 2000, Some(2000), true));
        assert_eq!(Some(d.topup_tip), default_tip(P, 100_000_000));
        // A stored T is reused, not renewed, and --reset-threshold renews it.
        let stored = Request { threshold: Some(777), ..r.clone() };
        let d = resolve(&order(Class::Resting), &stored, &mut v);
        assert_eq!((d.recv_threshold, d.threshold_new), (777, false));
        let d = resolve(&order(Class::Resting), &Request { reset: true, ..stored }, &mut v);
        assert_eq!((d.recv_threshold, d.threshold_new), (2000, true));
        // A minimum fill of 1 offers none; a tip above the carrier is refused.
        let mut v = vec![];
        resolve(&Order { min_fill: 1, ..order(Class::Resting) }, &r, &mut v);
        assert_eq!(v[0].code, "DELIVERY_ONE_UTXO_UNAVAILABLE");
        let mut v = vec![];
        resolve(&order(Class::Resting), &Request { tip: Some(200_000_000), ..r.clone() }, &mut v);
        assert_eq!(v[0].code, "TOPUP_TIP_INVALID");
        // A pair order never gets one-utxo.
        let mut v = vec![];
        // With a price the pair rule alone refuses it (a pair order has none in practice).
        let d = resolve(&Order { pair: true, ..order(Class::Resting) }, &r, &mut v);
        assert_eq!((d, v[0].code), (Resolved::EACH, "DELIVERY_ONE_UTXO_UNAVAILABLE"));
    }

    #[test]
    fn the_pair_tip_pays_the_custody_input_of_later_fills() {
        // 1.25 x 1,282,600 = 1,603,250 per later fill; 3 later fills over 4 whole tokens.
        assert_eq!(collected_pair_tip_extra(1000, 4000, 1000), (1_603_250i128 * 3 * 1000 / 4000 + 1) as i64);
        assert_eq!(collected_pair_tip_extra(1000, 4000, 4000), 0, "one possible fill: no later fill");
        let big = collected_pair_tip_extra(1000, 1_000_000, 1);
        assert_eq!(big, (1_603_250i128 * 63 * 1000 + 999_999) as i64 / 1_000_000, "at most 63 later fills");
    }
}
