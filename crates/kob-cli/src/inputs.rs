//! The order files the order commands read: the web wallet's backup (`kob-backup`), the indexer recovery / export file
//! (`docs/ops/executor.md` 7.5) and indexer order views (`GET /v1/orders/{id}`, or a page `{"items": [..]}`).
//!
//! Nothing in a file is trusted beyond availability: every state is decoded canonically under its template and every
//! UTXO is looked up on the node before anything is built.

use std::path::Path;

use kob_protocol::artifacts::TemplateId;
use kob_protocol::json::{from_hex, hex32, to_hex};
use kob_protocol::retired::lot::LotState;
use kob_protocol::state::{AnyState, TokenState};
use serde_json::Value;

use crate::common::{read_json, OrderState, Tpl};

/// One order of a backup or export file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `file#index` (or `file` for a single view).
    pub source: String,
    /// Covenant id.
    pub covenant_id: [u8; 32],
    /// Template hash (pinned or retired).
    pub template_hash: [u8; 32],
    /// Known state spans, the latest first (a proven later state, then the given one).
    pub spans: Vec<Vec<u8>>,
    /// Extension commitment of the order's token (ask side: the custody's), when the file knows it.
    pub ext: Option<[u8; 32]>,
    /// The maker the file claims (backup records).
    pub claimed_maker: Option<[u8; 32]>,
    /// The kind the file claims (backup records).
    pub claimed_kind: Option<String>,
    /// The entry names a state later than the placement (a backup record's `last`, an export's current state, a proven view).
    pub later: bool,
}

/// A token UTXO of an indexer view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenView {
    /// Outpoint transaction id.
    pub txid: [u8; 32],
    /// Outpoint index.
    pub index: u32,
    /// Token covenant id.
    pub token: [u8; 32],
    /// Owner.
    pub owner: [u8; 32],
    /// Token base units.
    pub amount: i64,
    /// KAS carrier (sompi).
    pub value: u64,
    /// DAA score of the creating block.
    pub created_daa: u64,
    /// Spent.
    pub spent: bool,
    /// The proven token state.
    pub state: Option<TokenState>,
    /// The token program the state was proven under.
    pub program: Option<TemplateId>,
    /// A token other than the order's own.
    pub foreign: bool,
}

/// An indexer order view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// `file` or `file#index`.
    pub source: String,
    /// Covenant id.
    pub covenant_id: [u8; 32],
    /// Template hash.
    pub template_hash: [u8; 32],
    /// Status the indexer reports.
    pub status: String,
    /// The proven current state (`state_known`): in the order's own layout, a retired template's in its lot layout.
    pub state: Option<OrderState>,
    /// The current UTXO: txid, index, value.
    pub current: Option<([u8; 32], u32, Option<u64>)>,
    /// DAA score of the current UTXO.
    pub current_daa: Option<u64>,
    /// Extension commitment of the token.
    pub ext: Option<[u8; 32]>,
    /// The live custody UTXO (a pair order: of its first custody).
    pub custody: Option<TokenView>,
    /// A pair order's second live custody UTXO (a sell-first `KobIfdPair`'s B prefund: `pair.custodies[1].utxo`).
    pub custody_b: Option<TokenView>,
    /// Live strays.
    pub strays: Vec<TokenView>,
}

impl View {
    /// The view as a recovery entry (its proven state, if any).
    pub fn entry(&self) -> Entry {
        let spans = match (&self.state, Tpl::resolve(&self.template_hash)) {
            (Some(s), Some(t)) => t.encode(s).map(|b| vec![b]).unwrap_or_default(),
            _ => vec![],
        };
        Entry {
            source: self.source.clone(),
            covenant_id: self.covenant_id,
            template_hash: self.template_hash,
            later: !spans.is_empty(),
            spans,
            ext: self.ext,
            claimed_maker: None,
            claimed_kind: None,
        }
    }
}

/// A file entry that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    /// `file#index`.
    pub source: String,
    /// Why.
    pub reason: String,
}

/// What one file holds.
#[derive(Debug, Default)]
pub struct Parsed {
    /// Backup / export entries.
    pub entries: Vec<Entry>,
    /// Indexer views.
    pub views: Vec<View>,
    /// Entries that could not be read.
    pub rejected: Vec<Rejected>,
}

fn obj<'a>(v: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("{what} is not an object"))
}

fn h32(v: Option<&Value>, what: &str) -> Result<[u8; 32], String> {
    let s = v.and_then(Value::as_str).ok_or_else(|| format!("{what} is missing"))?;
    hex32(s).map_err(|e| format!("{what}: {e}"))
}

fn opt_h32(v: Option<&Value>, what: &str) -> Result<Option<[u8; 32]>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(x) => h32(Some(x), what).map(Some),
    }
}

fn span(v: Option<&Value>, what: &str) -> Result<Vec<u8>, String> {
    let s = v.and_then(Value::as_str).ok_or_else(|| format!("{what} is missing"))?;
    let b = from_hex(s).map_err(|e| format!("{what}: {e}"))?;
    if b.is_empty() {
        return Err(format!("{what} is empty"));
    }
    Ok(b)
}

fn int(v: Option<&Value>, what: &str) -> Result<Option<i128>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_i64().map(|x| Some(x as i128)).ok_or_else(|| format!("{what}: not an integer")),
        Some(Value::String(s)) => s.parse::<i128>().map(Some).map_err(|_| format!("{what}: not an integer: {s}")),
        Some(other) => Err(format!("{what}: not an integer: {other}")),
    }
}

/// Read one file of any of the three formats. `network` is checked against the file's own (backup and export files).
pub fn read_file(path: &Path, network: &str) -> Result<Parsed, String> {
    let v = read_json(path)?;
    parse(&v, &path.display().to_string(), network)
}

/// Parse a document of any of the three formats.
pub fn parse(v: &Value, name: &str, network: &str) -> Result<Parsed, String> {
    let o = obj(v, name)?;
    let check_network = |n: Option<&Value>| -> Result<(), String> {
        match n.and_then(Value::as_str) {
            Some(n) if n == network => Ok(()),
            Some(n) => Err(format!("{name} is for network {n}, not {network}")),
            None => Err(format!("{name} names no network")),
        }
    };
    if o.get("format").and_then(Value::as_str) == Some("kob-backup") {
        if o.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(format!("{name}: not a kob-backup version 1 file"));
        }
        check_network(o.get("network"))?;
        let records = o.get("records").and_then(Value::as_array).ok_or_else(|| format!("{name}: no records"))?;
        let mut p = Parsed::default();
        for (i, r) in records.iter().enumerate() {
            let source = format!("{name}#{i}");
            match backup_record(r, network) {
                Ok(mut e) => {
                    e.source = source;
                    p.entries.push(e)
                }
                Err(reason) => p.rejected.push(Rejected { source, reason }),
            }
        }
        return Ok(p);
    }
    if let Some(orders) = o.get("orders") {
        match o.get("version").and_then(Value::as_u64) {
            Some(1 | 2) => {}
            _ => return Err(format!("{name}: not a version 1 or 2 recovery file")),
        }
        check_network(o.get("network"))?;
        let orders = orders.as_array().ok_or_else(|| format!("{name}: orders is not an array"))?;
        let mut p = Parsed::default();
        for (i, e) in orders.iter().enumerate() {
            let source = format!("{name}#{i}");
            match export_entry(e) {
                Ok(mut e) => {
                    e.source = source;
                    p.entries.push(e)
                }
                Err(reason) => p.rejected.push(Rejected { source, reason }),
            }
        }
        return Ok(p);
    }
    if let Some(items) = o.get("items") {
        let items = items.as_array().ok_or_else(|| format!("{name}: items is not an array"))?;
        let mut p = Parsed::default();
        for (i, it) in items.iter().enumerate() {
            let source = format!("{name}#{i}");
            match view(it, &source) {
                Ok(v) => p.views.push(v),
                Err(reason) => p.rejected.push(Rejected { source, reason }),
            }
        }
        return Ok(p);
    }
    if o.contains_key("covenant_id") && o.contains_key("template_hash") {
        let mut p = Parsed::default();
        match view(v, name) {
            Ok(v) => p.views.push(v),
            Err(reason) => p.rejected.push(Rejected { source: name.to_string(), reason }),
        }
        return Ok(p);
    }
    Err(format!("{name}: not a kob-backup, a recovery / export file or an indexer order view"))
}

fn backup_record(r: &Value, network: &str) -> Result<Entry, String> {
    let o = obj(r, "record")?;
    if o.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("record version is not 1".into());
    }
    if o.get("network").and_then(Value::as_str) != Some(network) {
        return Err("record is for another network".into());
    }
    let state = span(o.get("state"), "state")?;
    let last = match o.get("last") {
        None | Some(Value::Null) => None,
        Some(l) => Some(span(obj(l, "last")?.get("state"), "last.state")?),
    };
    let custody_ext = match o.get("custody").and_then(|c| c.get("tokenState")) {
        None | Some(Value::Null) => None,
        Some(ts) => {
            Some(serde_json::from_value::<TokenState>(ts.clone()).map_err(|e| format!("custody.tokenState: {e}"))?.extension())
        }
    };
    let ext = opt_h32(o.get("ext"), "ext")?.or(custody_ext);
    let mut spans = vec![];
    spans.extend(last);
    if !spans.contains(&state) {
        spans.push(state);
    }
    Ok(Entry {
        source: String::new(),
        covenant_id: h32(o.get("covenantId"), "covenantId")?,
        template_hash: h32(o.get("templateHash"), "templateHash")?,
        later: spans.len() > 1,
        spans,
        ext,
        claimed_maker: Some(h32(o.get("maker"), "maker")?),
        claimed_kind: Some(o.get("kind").and_then(Value::as_str).ok_or("kind is missing")?.to_string()),
    })
}

fn export_entry(e: &Value) -> Result<Entry, String> {
    let o = obj(e, "entry")?;
    let covenant_id = match opt_h32(o.get("covenant_id"), "covenant_id")? {
        Some(c) => c,
        None => return Err("covenant_id is missing (an entry without it cannot be verified on the node)".into()),
    };
    Ok(Entry {
        source: String::new(),
        covenant_id,
        template_hash: h32(o.get("template_hash"), "template_hash")?,
        spans: vec![span(o.get("state"), "state")?],
        ext: opt_h32(o.get("extension_commitment"), "extension_commitment")?,
        claimed_maker: None,
        claimed_kind: None,
        // the export carries the current script when the exporter knew it (executor.md 7.5)
        later: true,
    })
}

fn token_view(v: &Value, what: &str) -> Result<TokenView, String> {
    let o = obj(v, what)?;
    let index = o.get("index").and_then(Value::as_u64).filter(|i| *i <= u32::MAX as u64).ok_or(format!("{what}.index"))?;
    let amount = int(o.get("amount"), &format!("{what}.amount"))?.ok_or(format!("{what}.amount is missing"))?;
    let value = int(o.get("value"), &format!("{what}.value"))?.ok_or(format!("{what}.value is missing"))?;
    let state = match o.get("state") {
        None | Some(Value::Null) => None,
        Some(s) => Some(serde_json::from_value::<TokenState>(s.clone()).map_err(|e| format!("{what}.state: {e}"))?),
    };
    let program = match o.get("program") {
        None | Some(Value::Null) => None,
        Some(p) => {
            let name = p.as_str().ok_or(format!("{what}.program is not a string"))?;
            Some(
                TemplateId::from_name(name)
                    .filter(|t| t.is_token())
                    .ok_or(format!("{what}.program `{name}` is not a token program"))?,
            )
        }
    };
    Ok(TokenView {
        txid: h32(o.get("txid"), &format!("{what}.txid"))?,
        index: index as u32,
        token: h32(o.get("token"), &format!("{what}.token"))?,
        owner: h32(o.get("owner"), &format!("{what}.owner"))?,
        amount: i64::try_from(amount).map_err(|_| format!("{what}.amount out of range"))?,
        value: u64::try_from(value).map_err(|_| format!("{what}.value out of range"))?,
        created_daa: int(o.get("created_daa"), "created_daa")?.unwrap_or(0).max(0) as u64,
        spent: o.get("spent").and_then(Value::as_bool).unwrap_or(false),
        state,
        program,
        foreign: o.get("foreign").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn view(v: &Value, source: &str) -> Result<View, String> {
    let o = obj(v, "order view")?;
    let state_known = o.get("state_known").and_then(Value::as_bool).unwrap_or(false);
    let template_hash = h32(o.get("template_hash"), "template_hash")?;
    let state = match o.get("state") {
        Some(s) if state_known && !s.is_null() => Some(match Tpl::resolve(&template_hash) {
            // an order of a retired template: its state is in the older lot layout
            Some(Tpl::Retired(r)) if r.is_current_layout() => {
                // a retired template with today's layout: today's state type (cancel only, not validated)
                let s = serde_json::from_value::<AnyState>(s.clone()).map_err(|e| format!("state: {e}"))?;
                OrderState::RetiredCurrent(s)
            }
            Some(Tpl::Retired(r)) => {
                match serde_json::from_value::<LotState>(s.clone()) {
                    Ok(l) => OrderState::Lot(l, r.family),
                    // the protocol v3 cross limit (no lots)
                    Err(e) => OrderState::NoLot(
                        serde_json::from_value::<kob_protocol::retired::nolot::CrossState>(s.clone())
                            .map_err(|_| format!("state: {e}"))?,
                    ),
                }
            }
            _ => {
                let s = serde_json::from_value::<AnyState>(s.clone()).map_err(|e| format!("state: {e}"))?;
                s.validate().map_err(|e| format!("state: {e}"))?;
                OrderState::Current(s)
            }
        }),
        _ => None,
    };
    let current = match o.get("current") {
        None | Some(Value::Null) => None,
        Some(c) => {
            let c = obj(c, "current")?;
            let index = c.get("index").and_then(Value::as_u64).filter(|i| *i <= u32::MAX as u64).ok_or("current.index")?;
            let value = int(c.get("value"), "current.value")?.map(|x| u64::try_from(x).map_err(|_| "current.value out of range"));
            Some((h32(c.get("txid"), "current.txid")?, index as u32, value.transpose()?))
        }
    };
    let custody = match o.get("custody").and_then(|c| c.get("utxo")) {
        None | Some(Value::Null) => None,
        Some(u) => Some(token_view(u, "custody.utxo")?),
    };
    // a pair order view lists its custodies under `pair.custodies` (record order); the top-level `custody` is the first
    let custody_b = match o.get("pair").and_then(|p| p.get("custodies")).and_then(|c| c.get(1)).and_then(|c| c.get("utxo")) {
        None | Some(Value::Null) => None,
        Some(u) => Some(token_view(u, "pair.custodies[1].utxo")?),
    };
    let strays = match o.get("strays") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => {
            a.iter().enumerate().map(|(i, s)| token_view(s, &format!("strays[{i}]"))).collect::<Result<_, _>>()?
        }
        Some(_) => return Err("strays is not an array".into()),
    };
    Ok(View {
        source: source.to_string(),
        covenant_id: h32(o.get("covenant_id"), "covenant_id")?,
        template_hash,
        status: o.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
        state,
        current,
        current_daa: int(o.get("current_daa"), "current_daa")?.map(|d| d.max(0) as u64),
        ext: opt_h32(o.get("extension_commitment"), "extension_commitment")?,
        custody,
        custody_b,
        strays,
    })
}

/// Merge entries of one covenant id: entries with a later state first; every distinct span of the same template is kept
/// as a candidate (the node decides which, if any, is the order's script).
pub fn merge(entries: Vec<Entry>) -> (Vec<Entry>, Vec<Rejected>) {
    let mut out: Vec<Entry> = vec![];
    let mut notes = vec![];
    let mut sorted = entries;
    sorted.sort_by_key(|e| !e.later);
    for e in sorted {
        match out.iter_mut().find(|x| x.covenant_id == e.covenant_id) {
            None => out.push(e),
            Some(x) if x.template_hash != e.template_hash => notes.push(Rejected {
                source: e.source.clone(),
                reason: format!("covenant id {} is also in {} under another template hash: ignored", to_hex(&e.covenant_id), x.source),
            }),
            Some(x) => {
                for s in e.spans {
                    if !x.spans.contains(&s) {
                        x.spans.push(s);
                    }
                }
                x.ext = x.ext.or(e.ext);
                x.claimed_maker = x.claimed_maker.or(e.claimed_maker);
                x.claimed_kind = x.claimed_kind.take().or(e.claimed_kind);
                x.source = format!("{}, {}", x.source, e.source);
            }
        }
    }
    (out, notes)
}
