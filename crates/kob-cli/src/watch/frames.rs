//! How `kob watch` words the frames of the executor, one human line per event.
//!
//! The frames are described in `docs/api-reference.md`, section 4.2. The functions are pure, with
//! no clock and no I/O.

use serde_json::Value;

use crate::trade::units::{self, TokenUnit};

/// What a fill line needs of a token to show its amount and price in whole units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    /// The ticker, empty when there is none.
    pub ticker: String,
    /// The decimals.
    pub decimals: u32,
    /// The base units that the order prices are quoted per, see `TokenMarket::scale`.
    pub scale: i64,
}

/// `HH:MM:SS` in UTC of a unix time in seconds.
pub fn clock(unix_secs: u64) -> String {
    let s = unix_secs % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// A number that the executor writes as a decimal string or a JSON number.
fn int(v: &Value, k: &str) -> Option<i128> {
    match v.get(k)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64().map(i128::from),
        _ => None,
    }
}

fn opt(v: Option<i128>) -> String {
    v.map_or("-".into(), |x| x.to_string())
}

/// The line of the `data` of a `fill` frame.
///
/// `info` names the ticker, decimals and price scale of the token when the lists know them.
pub fn fill_line(data: &Value, info: Option<&TokenInfo>) -> String {
    let side = match data.get("side").and_then(Value::as_i64) {
        Some(1) => "ask",
        Some(2) => "bid",
        _ => "?",
    };
    let token = text(data, "token").map(str::to_string);
    let amount = int(data, "amount");
    let price = int(data, "price");
    let (amount_text, price_text) = match info {
        Some(t) => {
            let unit = TokenUnit { ticker: t.ticker.clone(), decimals: t.decimals };
            let name = if t.ticker.is_empty() { token.clone().unwrap_or_else(|| "token".into()) } else { t.ticker.clone() };
            let a = amount.map_or("?".to_string(), |a| format!("{} {name}", units::fmt_decimal(a, t.decimals)));
            let p = match price.and_then(|p| i64::try_from(p).ok()) {
                Some(p) if t.scale > 0 => units::price_per_whole(p, &unit, t.scale, units::KAS_DECIMALS, "KAS"),
                Some(p) => format!("raw price {p}"),
                None => "no price".to_string(),
            };
            (a, p)
        }
        None => (
            format!("{} base units of {}", amount.map_or("?".into(), |a| a.to_string()), token.clone().unwrap_or_else(|| "?".into())),
            price.map_or("no price".to_string(), |p| format!("raw price {p} (decimals unknown)")),
        ),
    };
    let mut line = format!("fill  {side:<3}  {amount_text} @ {price_text}");
    if let Some(p) = int(data, "payout") {
        line.push_str(&format!("  paid {}", units::kas(p)));
    }
    line.push_str(&format!("  order {}", text(data, "order").unwrap_or("?")));
    line.push_str(&format!("  tx {}", text(data, "txid").unwrap_or("?")));
    if let Some(d) = int(data, "daa") {
        line.push_str(&format!("  daa {d}"));
    }
    line
}

/// The line of a `cursor` frame.
pub fn cursor_line(data: &Value) -> String {
    format!(
        "cursor  daa {}  +{} -{} blocks",
        opt(int(data, "cursor_daa")),
        int(data, "added_blocks").unwrap_or(0),
        int(data, "reverted_blocks").unwrap_or(0)
    )
}

/// The line of a `health` frame.
pub fn health_line(data: &Value) -> String {
    let mut s = format!(
        "health  {}  daa {}  node {}  lag {}",
        text(data, "state").unwrap_or("?"),
        opt(int(data, "cursor_daa")),
        opt(int(data, "node_daa")),
        opt(int(data, "lag_daa"))
    );
    // The update notice of the executor says that a newer KOB release is in use on chain. It is
    // shown only as a release version, never as raw text.
    let newer = data.get("newer_release_seen").and_then(|n| text(n, "version"));
    if let Some(v) = newer.and_then(|v| v.parse::<kob_protocol::payload::Release>().ok()) {
        s.push_str(&format!("  | a newer KOB release is in use on chain ({v}): update"));
    }
    s
}

/// The note of a `reorg` frame.
pub fn reorg_line(data: &Value) -> String {
    format!(
        "reorg: {} block(s) reverted, {} added (daa {}); fills seen so far may have been reverted",
        int(data, "reverted_blocks").unwrap_or(0),
        int(data, "added_blocks").unwrap_or(0),
        opt(int(data, "cursor_daa"))
    )
}

/// The note of a `resync` frame.
pub fn resync_line(data: &Value) -> String {
    format!("resync: this client missed {} events (it fell behind the server's buffer)", opt(int(data, "missed_events")))
}

/// A line for a frame that this build has no wording for, such as one of a newer executor.
///
/// It shows the type, channel and data on one line.
pub fn generic_line(frame: &Value) -> String {
    let ty = text(frame, "type").unwrap_or("?");
    let ch = text(frame, "channel").map(|c| format!(" [{c}]")).unwrap_or_default();
    format!("{ty}{ch}  {}", frame.get("data").map_or(String::new(), Value::to_string))
}

/// What the `subscribed` reply says.
///
/// That is the channels held now and the errors for names that failed.
pub fn subscribed(frame: &Value) -> (Vec<String>, Vec<String>) {
    let list = |k: &str| {
        frame
            .pointer(&format!("/data/{k}"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    (list("channels"), list("errors"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tusd() -> TokenInfo {
        TokenInfo { ticker: "TUSD".into(), decimals: 3, scale: 1000 }
    }

    #[test]
    fn the_clock() {
        assert_eq!(clock(0), "00:00:00");
        assert_eq!(clock(1_793_761_724), "03:08:44");
    }

    #[test]
    fn a_fill_in_whole_units() {
        let d = json!({"order": "6a".repeat(32), "token": "55".repeat(32), "side": 1, "price": "250000000", "amount": "2500",
                       "payout": "625000000", "txid": "62".repeat(32), "block": "00".repeat(32), "daa": 995});
        assert_eq!(
            fill_line(&d, Some(&tusd())),
            format!(
                "fill  ask  2.5 TUSD @ 2.5 KAS per TUSD  paid 6.25 KAS  order {}  tx {}  daa 995",
                "6a".repeat(32),
                "62".repeat(32)
            )
        );
        // A bid, with no payout for a conditional kind and no price for a pair order.
        let d = json!({"order": "6a".repeat(32), "token": null, "side": 2, "price": null, "amount": "1", "payout": null,
                       "txid": "62".repeat(32), "daa": 7});
        let l = fill_line(&d, Some(&tusd()));
        assert!(l.starts_with("fill  bid  0.001 TUSD @ no price  order "), "{l}");
        assert!(!l.contains("paid"), "{l}");
    }

    #[test]
    fn a_fill_of_a_token_the_lists_do_not_name() {
        let d = json!({"order": "6a".repeat(32), "token": "55".repeat(32), "side": 1, "price": "3333445595", "amount": "99980049",
                       "txid": "62".repeat(32), "daa": 1});
        let l = fill_line(&d, None);
        assert!(
            l.starts_with("fill  ask  99980049 base units of 5555555555555555555555555555555555555555555555555555555555555555 @ raw price 3333445595 (decimals unknown)"),
            "{l}"
        );
        // A token known by decimals only is named by its full id.
        let info = TokenInfo { ticker: String::new(), decimals: 2, scale: 100 };
        assert!(fill_line(&d, Some(&info))
            .contains("999800.49 5555555555555555555555555555555555555555555555555555555555555555 @ 33.33445595 KAS per token"));
    }

    #[test]
    fn control_lines() {
        assert_eq!(
            cursor_line(&json!({"cursor_hash": "x", "cursor_daa": 995, "added_blocks": 2, "reverted_blocks": 0})),
            "cursor  daa 995  +2 -0 blocks"
        );
        assert_eq!(
            health_line(&json!({"state": "following", "cursor_daa": 995, "node_daa": null, "lag_daa": null})),
            "health  following  daa 995  node -  lag -"
        );
        // The update notice of the executor. Anything that is not a release version is not printed.
        let notice = json!({"state": "following", "cursor_daa": 995, "newer_release_seen": {"version": "0.1.1", "first_seen_daa": 7}});
        assert_eq!(
            health_line(&notice),
            "health  following  daa 995  node -  lag -  | a newer KOB release is in use on chain (0.1.1): update"
        );
        let odd = json!({"state": "following", "cursor_daa": 995, "newer_release_seen": {"version": "\u{1b}[2J9.9.9"}});
        assert_eq!(health_line(&odd), "health  following  daa 995  node -  lag -");
        assert!(reorg_line(&json!({"reverted_blocks": 1, "added_blocks": 2, "cursor_daa": 9}))
            .starts_with("reorg: 1 block(s) reverted, 2 added (daa 9)"));
        assert!(resync_line(&json!({"missed_events": 12})).contains("missed 12 events"));
        assert_eq!(generic_line(&json!({"channel": "orders", "type": "create", "data": {"a": 1}})), r#"create [orders]  {"a":1}"#);
    }

    #[test]
    fn the_subscribed_reply() {
        let f = json!({"type": "subscribed", "data": {"channels": ["fills"], "errors": ["unknown channel `x`"]}});
        assert_eq!(subscribed(&f), (vec!["fills".to_string()], vec!["unknown channel `x`".to_string()]));
        assert_eq!(subscribed(&json!({"type": "subscribed"})), (vec![], vec![]));
    }
}
