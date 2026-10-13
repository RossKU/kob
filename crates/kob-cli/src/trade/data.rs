//! What the trading commands read from the node and the indexer (`web/src/app/env.ts`): the clock, the maker's KAS and
//! token UTXOs, the book, the maker's own orders, the last fill, and the live order an action spends. Every UTXO the
//! indexer names is looked up on the node (exact script, covenant id, outpoint) before anything is built from it.

use std::collections::BTreeMap;
use std::time::Duration;

use kob_protocol::artifacts::token_template;
use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{KeyUtxo, OrderUtxo, TokenUtxo};
use serde_json::{json, Value};

use super::http;
use super::market::{self, TokenMarket};
use super::plan::{Book, BookOrder, Clock, Level, OwnOrder, Side};
use crate::common::{lookup, maker_funding, order_spk, order_template, spk_bytes, utxo_of, NODE_TIMEOUT};
use crate::inputs;

/// Timeout of one indexer request.
pub const INDEXER_TIMEOUT: Duration = Duration::from_secs(30);

/// The node's virtual DAA score and the local UTC clock.
pub fn clock(node: &str) -> Result<Clock, String> {
    let p = crate::wrpc::call(node, "getBlockDagInfo", json!({}), NODE_TIMEOUT)
        .map_err(|e| format!("node {}: {e}", crate::wrpc::shown(node)))?;
    let daa = match p.get("virtualDaaScore") {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("node {}: no virtualDaaScore in getBlockDagInfo", crate::wrpc::shown(node)))?;
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    Ok(Clock { daa, unix, rate_milli: 10_000 })
}

/// The maker's plain KAS UTXOs.
pub fn funding(node: &str, network: &str, maker: &[u8; 32]) -> Result<Vec<KeyUtxo>, String> {
    maker_funding(node, network, maker)
}

/// The token list: the registry (`--registry`, else the embedded one when its network matches) and the indexer's.
pub fn token_lists(
    registry: Option<&std::path::Path>,
    network: &str,
    indexer: Option<&str>,
) -> Result<(Vec<market::TokenFacts>, Vec<market::TokenFacts>), String> {
    let reg = match registry {
        Some(p) => {
            market::registry_tokens(&std::fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?, network)
                .map_err(|e| format!("{}: {e}", p.display()))?
        }
        None => {
            let r = kob_protocol::registry::DEFAULT_REGISTRY_JSON;
            let net = serde_json::from_str::<Value>(r).ok().and_then(|v| v.get("network").and_then(Value::as_str).map(str::to_owned));
            if net.as_deref() == Some(network) {
                market::registry_tokens(r, network)?
            } else {
                vec![]
            }
        }
    };
    let idx = match indexer {
        Some(u) => market::indexer_tokens(&http::get_ok(u, "/v1/tokens", INDEXER_TIMEOUT)?)?,
        None => vec![],
    };
    Ok((reg, idx))
}

fn i64_of(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// The maker's token UTXOs of `m`: listed by the indexer (`GET /v1/token-utxos`) or by the record of the key's own recent
/// transactions ([`crate::recent`]: outputs the indexer may not have seen yet), each confirmed on the node.
pub fn token_utxos(indexer: &str, node: &str, network: &str, m: &TokenMarket, owner: &[u8; 32]) -> Result<Vec<TokenUtxo>, String> {
    let path = format!("/v1/token-utxos?owner={}&token={}&limit=500", to_hex(owner), to_hex(&m.cov));
    let v = http::get_ok(indexer, &path, INDEXER_TIMEOUT)?;
    let items = v.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let tt = token_template(m.program);
    let mut cands: Vec<(TokenState, [u8; 32], u32)> = vec![];
    for it in items {
        if it.get("spent").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(st) = it.get("state").and_then(|s| serde_json::from_value::<TokenState>(s.clone()).ok()) else { continue };
        if st.family() != m.family || !st.is_holder(m.program) || st.owner() != *owner {
            continue;
        }
        let (Some(txid), Some(index)) = (
            it.get("txid").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32),
            it.get("index").and_then(Value::as_u64),
        ) else {
            continue;
        };
        cands.push((st, txid, index as u32));
    }
    // the key's own outputs the indexer has not seen yet (its token change of a transaction just sent): candidates like the
    // indexer's, confirmed on the node below
    for (st, txid, index) in crate::recent::candidates(network, owner, &m.cov) {
        if st.family() == m.family && st.owner() == *owner && !cands.iter().any(|c| c.1 == txid && c.2 == index) {
            cands.push((st, txid, index));
        }
    }
    let spks: Vec<_> = cands.iter().map(|c| c.0.spk_with(tt)).collect();
    let at = lookup(node, network, &spks)?;
    let mut out = vec![];
    for ((st, txid, index), spk) in cands.into_iter().zip(spks) {
        let found = at
            .get(&spk_bytes(&spk))
            .and_then(|v| v.iter().find(|u| u.txid == txid && u.index == index && u.covenant_id == Some(m.cov)));
        if let Some(u) = found {
            out.push(TokenUtxo { utxo: utxo_of(u), state: st });
        }
    }
    Ok(out)
}

/// The standard scale of the listed orders of `cov` (`market::standard_scale` of its per-order book), `None` without orders:
/// the decimals of a token without a registry entry.
pub fn order_scale(indexer: &str, cov: &[u8; 32]) -> Result<Option<i64>, String> {
    let v = http::get_ok(indexer, &format!("/v1/books/{}?depth=100&aggregate=false", to_hex(cov)), INDEXER_TIMEOUT)?;
    Ok(market::standard_scale(&v))
}

/// The KAS book of `m` (per-order rows; orders of another scale are not part of it).
pub fn book(indexer: &str, m: &TokenMarket) -> Result<Book, String> {
    let v = http::get_ok(indexer, &format!("/v1/books/{}?depth=100&aggregate=false", to_hex(&m.cov)), INDEXER_TIMEOUT)?;
    Ok(book_from_indexer(m.scale, &v))
}

/// The indexer book view (per order or aggregated) as the planners' book (`bookFromIndexer`).
pub fn book_from_indexer(scale: i64, v: &Value) -> Book {
    let side = |rows: Option<&Vec<Value>>| -> (Vec<Level>, Option<Vec<BookOrder>>) {
        let rows = rows.cloned().unwrap_or_default();
        let same = |r: &Value| r.get("scale").and_then(Value::as_i64).is_none_or(|s| s == scale);
        if rows.iter().all(|r| r.get("covenant_id").is_some()) && !rows.is_empty() {
            let mut orders = vec![];
            for r in rows.iter().filter(|r| same(r)) {
                let Some(price) = i64_of(r.get("price")) else { continue };
                orders.push(BookOrder {
                    price,
                    amount: i64_of(r.get("amount_left")).unwrap_or(1),
                    tip: i64_of(r.get("tip")).unwrap_or(0),
                    min_fill: i64_of(r.get("min_fill")),
                });
            }
            let mut levels: Vec<Level> = vec![];
            for o in &orders {
                match levels.last_mut() {
                    Some(l) if l.price == o.price => {
                        l.amount = l.amount.saturating_add(o.amount);
                        l.orders = l.orders.saturating_add(1);
                    }
                    _ => levels.push(Level { price: o.price, amount: o.amount, orders: 1, tip: 0 }),
                }
            }
            (levels, Some(orders))
        } else {
            let mut levels: Vec<Level> = vec![];
            for r in rows.iter().filter(|r| same(r)) {
                let (Some(price), amount) = (i64_of(r.get("price")), i64_of(r.get("amount")).unwrap_or(0)) else { continue };
                let n = r.get("orders").and_then(Value::as_i64).unwrap_or(1);
                match levels.iter_mut().find(|l| l.price == price) {
                    Some(l) => {
                        l.amount = l.amount.saturating_add(amount);
                        l.orders = l.orders.saturating_add(n);
                    }
                    None => levels.push(Level { price, amount, orders: n, tip: 0 }),
                }
            }
            (levels, if rows.is_empty() { Some(vec![]) } else { None })
        }
    };
    let (asks, ask_orders) = side(v.get("asks").and_then(Value::as_array));
    let (bids, bid_orders) = side(v.get("bids").and_then(Value::as_array));
    Book { asks, bids, ask_orders, bid_orders }
}

/// The maker's live orders of `m` in its KAS book (self-trade prevention; pair orders and unpriced stops left out).
pub fn own_orders(indexer: &str, m: &TokenMarket, maker: &[u8; 32]) -> Result<Vec<OwnOrder>, String> {
    let path = format!("/v1/orders?maker={}&token={}&status=active&limit=200", to_hex(maker), to_hex(&m.cov));
    let v = http::get_ok(indexer, &path, INDEXER_TIMEOUT)?;
    let mut out = vec![];
    for o in v.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
        let status = o.get("status").and_then(Value::as_str).unwrap_or("");
        if !(status == "open" || status == "partial") || o.get("pair").is_some_and(|p| !p.is_null()) {
            continue;
        }
        if o.get("token").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32) != Some(m.cov) {
            continue;
        }
        if o.get("scale").and_then(Value::as_i64).is_some_and(|s| s != m.scale) {
            continue;
        }
        let Some(price) = i64_of(o.get("quote")).or_else(|| i64_of(o.get("price"))) else { continue };
        let Some(cov) = o.get("covenant_id").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32) else { continue };
        out.push(OwnOrder {
            cov,
            side: if o.get("side").and_then(Value::as_i64) == Some(1) { Side::Sell } else { Side::Buy },
            price,
            tip: i64_of(o.get("tip")).unwrap_or(0),
            amount_left: i64_of(o.get("amount_left")).or_else(|| i64_of(o.get("initial_amount"))).unwrap_or(0),
            active: !o.get("expired").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(out)
}

/// The price of the newest KAS trade of `m` (per whole token of its scale).
pub fn last_fill(indexer: &str, m: &TokenMarket) -> Result<Option<i64>, String> {
    let v = http::get_ok(indexer, &format!("/v1/trades/{}?limit=1", to_hex(&m.cov)), INDEXER_TIMEOUT)?;
    let basis = i64_of(v.get("price_basis")).filter(|b| *b > 0);
    let price = v.get("items").and_then(Value::as_array).and_then(|a| a.first()).and_then(|t| i64_of(t.get("price")));
    Ok(match (price, basis) {
        (Some(p), Some(b)) => Some((p as i128 * m.scale as i128 / b as i128) as i64),
        _ => None,
    })
}

/// A KAS reference of `m` a far-off quote cannot move (`firmReferencePrice`): the bid at which the bids are worth 10 KAS,
/// midpointed with the best ask when that is at most twice the bid.
pub fn firm_reference(b: &Book, scale: i64) -> Option<i64> {
    let mut value = 0i128;
    let mut bid = None;
    for l in &b.bids {
        value = value.saturating_add(l.amount as i128 * l.price as i128 / scale as i128);
        if value >= 1_000_000_000 {
            bid = Some(l.price);
            break;
        }
    }
    let bid = bid.filter(|x| *x > 0)?;
    match b.asks.first() {
        Some(a) if a.price >= bid && a.price as i128 <= 2 * bid as i128 => Some(((a.price as i128 + bid as i128) / 2) as i64),
        _ => Some(bid),
    }
}

/// The pair book of A/B as a book in B per whole A (`pairBookToBookView`: asks rounded up, bids down).
pub fn pair_book(indexer: &str, a: &TokenMarket, b: &TokenMarket) -> Result<Book, String> {
    let v = http::get_ok(indexer, &format!("/v1/pairs/{}/{}/book?depth=100", to_hex(&a.cov), to_hex(&b.cov)), INDEXER_TIMEOUT)?;
    Ok(pair_book_from(&v, a.scale))
}

/// [`pair_book`] from its JSON.
pub fn pair_book_from(v: &Value, scale_a: i64) -> Book {
    let side = |rows: Option<&Vec<Value>>, up: bool| -> Vec<Level> {
        let mut out: Vec<Level> = vec![];
        for l in rows.cloned().unwrap_or_default() {
            let (Some(num), Some(den), Some(amount)) = (
                l.get("price_num").and_then(Value::as_str).and_then(|s| s.parse::<i128>().ok()),
                l.get("price_den").and_then(Value::as_str).and_then(|s| s.parse::<i128>().ok()),
                l.get("amount").and_then(Value::as_str).and_then(|s| s.parse::<i64>().ok()),
            ) else {
                continue;
            };
            if den <= 0 || amount <= 0 {
                continue;
            }
            // the indexer's numbers: an overflow drops the level
            let Some(n) = num.checked_mul(scale_a as i128) else { continue };
            let Some(price) = (if up { n.checked_add(den - 1).map(|x| x / den) } else { Some(n / den) }) else { continue };
            let Ok(price) = i64::try_from(price) else { continue };
            if price <= 0 {
                continue;
            }
            let orders = l.get("orders").and_then(Value::as_i64).filter(|o| *o > 0).unwrap_or(1);
            match out.last_mut() {
                Some(x) if x.price == price => {
                    x.amount = x.amount.saturating_add(amount);
                    x.orders = x.orders.saturating_add(orders);
                }
                _ => out.push(Level { price, amount, orders, tip: 0 }),
            }
        }
        if up {
            out.sort_by_key(|l| l.price);
        } else {
            out.sort_by_key(|l| std::cmp::Reverse(l.price));
        }
        out
    };
    Book {
        asks: side(v.get("asks").and_then(Value::as_array), true),
        bids: side(v.get("bids").and_then(Value::as_array), false),
        ..Default::default()
    }
}

/// The maker's live pair orders of A/B.
pub fn own_pair_orders(indexer: &str, a: &TokenMarket, b: &TokenMarket, maker: &[u8; 32]) -> Result<Vec<OwnOrder>, String> {
    let path = format!("/v1/orders?maker={}&token={}&status=active&limit=200", to_hex(maker), to_hex(&a.cov));
    let v = http::get_ok(indexer, &path, INDEXER_TIMEOUT)?;
    let mut out = vec![];
    for o in v.get("items").and_then(Value::as_array).cloned().unwrap_or_default() {
        let Some(p) = o.get("pair").filter(|p| !p.is_null()) else { continue };
        let status = o.get("status").and_then(Value::as_str).unwrap_or("");
        let h = |k: &str| p.get(k).and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32);
        if !(status == "open" || status == "partial") || h("base") != Some(a.cov) || h("quote") != Some(b.cov) {
            continue;
        }
        let Some(price) = i64_of(p.get("quote_now")).or_else(|| i64_of(p.get("price"))).filter(|x| *x > 0) else { continue };
        let Some(cov) = o.get("covenant_id").and_then(Value::as_str).and_then(kob_protocol::registry::parse_hex32) else { continue };
        out.push(OwnOrder {
            cov,
            side: if p.get("side").and_then(Value::as_str) == Some("ask") { Side::Sell } else { Side::Buy },
            price,
            tip: 0,
            amount_left: i64_of(p.get("amount_left")).or_else(|| i64_of(o.get("amount_left"))).unwrap_or(0),
            active: !o.get("expired").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------------ a live order

/// A live order confirmed on the node: its UTXO and state, its custodies and the strays of its own token(s).
#[derive(Debug, Clone)]
pub struct Live {
    /// Covenant id.
    pub cov: [u8; 32],
    /// The order UTXO.
    pub order: OrderUtxo<AnyState>,
    /// The custody (a pair order: the first of its custodies).
    pub custody: Option<TokenUtxo>,
    /// A sell-first pair entry's B prefund.
    pub prefund: Option<TokenUtxo>,
    /// Strays of its own token(s), confirmed on the node, with their extension commitment.
    pub strays: Vec<TokenUtxo>,
    /// Strays of other tokens (left where they are; only `kob cancel` returns them).
    pub foreign: usize,
    /// A day order's deadline (placement record, as the indexer reports it).
    pub deadline: Option<u64>,
    /// The DAA score of the order's placement (as the indexer reports it): tells a GTC from a dated order.
    pub genesis_daa: Option<i64>,
}

/// `GET /v1/orders/{id}`, then the order UTXO, its custodies and its own-token strays on the node.
pub fn live_order(indexer: &str, node: &str, network: &str, cov: &[u8; 32]) -> Result<Live, String> {
    let v = http::get_ok(indexer, &format!("/v1/orders/{}", to_hex(cov)), INDEXER_TIMEOUT)?;
    let parsed = inputs::parse(&v, &format!("indexer order {}", to_hex(cov)), network)?;
    let view = parsed.views.into_iter().next().ok_or_else(|| {
        format!(
            "the indexer's view of {} is not usable: {}",
            to_hex(cov),
            parsed.rejected.first().map(|r| r.reason.clone()).unwrap_or_default()
        )
    })?;
    if view.covenant_id != *cov {
        return Err("the indexer answered with another order".into());
    }
    let state = view.state.clone().ok_or("the indexer has not proven the order's current state")?;
    let (txid, index, _) = view.current.ok_or("the order has no current UTXO (it has ended)")?;
    let tpl = order_template(&view.template_hash).ok_or("the order's template is not pinned by this build")?;
    let ospk = order_spk(tpl, &state)?;
    // custodies and own strays
    let targets = crate::recover::custody_targets(&state, *cov, view.ext)?;
    let mut spks = vec![ospk.clone()];
    spks.extend(targets.iter().map(|t| t.1.clone()));
    let own_tokens: Vec<[u8; 32]> = match state.pair_tokens() {
        Some(p) => vec![p.a.cov_id, p.b.cov_id],
        None => vec![state.token_cov_id()],
    };
    let program_of = |t: [u8; 32]| -> Option<kob_protocol::artifacts::TemplateId> {
        match state.pair_tokens() {
            Some(p) => {
                let k = if p.a.cov_id == t { p.a } else { p.b };
                kob_protocol::artifacts::token_template_by_hash(&k.tpl_hash).map(|x| x.id)
            }
            None => state.token_tpl_hash().and_then(|h| kob_protocol::artifacts::token_template_by_hash(&h)).map(|x| x.id),
        }
    };
    let mut stray_cands = vec![];
    let mut foreign = 0;
    for s in view.strays.iter().filter(|s| !s.spent) {
        if !own_tokens.contains(&s.token) {
            foreign += 1;
            continue;
        }
        let Some(program) = program_of(s.token) else { continue };
        if let Ok((st, spk)) = crate::order::stray_script(s, program) {
            if st.owner() == *cov && st.is_covenant_owned() {
                stray_cands.push((s.clone(), st, spk));
            }
        }
    }
    spks.extend(stray_cands.iter().map(|c| c.2.clone()));
    let at: BTreeMap<Vec<u8>, Vec<crate::wrpc::NodeUtxo>> = lookup(node, network, &spks)?;
    let node_order = at
        .get(&spk_bytes(&ospk))
        .and_then(|us| us.iter().find(|u| u.txid == txid && u.index == index && u.covenant_id == Some(*cov)))
        .ok_or("stale view: the node holds no unspent output of the order at its current outpoint")?;
    let mut held = vec![];
    for (k, (st, spk, token)) in targets.iter().enumerate() {
        let tv = if k == 0 { &view.custody } else { &view.custody_b };
        let tv = tv.as_ref().ok_or("the view names no custody UTXO for this token-holding order")?;
        let u = at
            .get(&spk_bytes(spk))
            .and_then(|us| us.iter().find(|u| u.txid == tv.txid && u.index == tv.index && u.covenant_id == Some(*token)))
            .ok_or("the order's custody is not on the node (stale view)")?;
        held.push(TokenUtxo { utxo: utxo_of(u), state: st.clone() });
    }
    let mut strays = vec![];
    for (s, st, spk) in stray_cands {
        if let Some(u) = crate::order::on_node(&at, &spk, &s) {
            strays.push(TokenUtxo { utxo: u, state: st });
        }
    }
    let mut held = held.into_iter();
    Ok(Live {
        cov: *cov,
        order: OrderUtxo { utxo: utxo_of(node_order), state },
        custody: held.next(),
        prefund: held.next(),
        strays,
        foreign,
        deadline: v.get("deadline").and_then(Value::as_u64),
        genesis_daa: v.get("genesis").and_then(|g| i64_of(g.get("daa"))),
    })
}
