//! `kob recover`: find the maker's orders on the node from a backup / export / indexer view, without an indexer, and
//! optionally cancel them (`--cancel`).
//!
//! Only availability is trusted: every state is decoded canonically under its pinned template, its P2SH
//! script is derived here, and an order is live only when the node holds an unspent output at exactly that script carrying
//! the order's covenant id. A partial fill splices a new `amountLeft` into the script (a new address), and with amounts in
//! base units any amount at least `minFill` may have been filled, so the search over amounts is NOT exhaustive: it tries
//! the known states, then the amounts the maker names with `--amount-left` (from an indexer view), then a bounded
//! fallback of `amountLeft - k x minFill`. Ask-side custody is rebuilt from the live state (`amountLeft` base units owned
//! by the covenant id) and verified the same way. An order of a template this build does not pin is `unsupported`.

use std::path::PathBuf;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{token_template_by_hash, TemplateId};
use kob_protocol::build::{build, Action, CancelOrder};
use kob_protocol::json::{hex32, to_hex};
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{FeeOptions, OrderUtxo, TokenUtxo, MIN_FEE_RATE};
use kob_protocol::Family;
use serde_json::{json, Value};

use crate::common::{
    amount_is_the_only_mutable_field, amount_left_in, build_funded, decode_state, encode_state, kas, lookup, order_spk,
    order_template, outpoint_str, parse_amount, script_is_fixed, sign_validate_send, spk_bytes, utxo_of, with_amount_left, write_json,
    Funding, Signer,
};
use crate::inputs::{self, Entry, Rejected};
use crate::wrpc::NodeUtxo;

/// Usage text of `kob recover`.
pub const USAGE: &str = "\
kob recover: find orders on the node from a backup, a recovery / export file or an indexer view, and optionally cancel them

USAGE
  kob recover --from <FILE>... --node ws://HOST:PORT [--network NET] [--maker <PUBKEY>] [--out <report.json>]
              [--amount-left [<COVENANT ID>=]<AMOUNT>]...
              [--cancel --key <HEX | env:VAR | file:PATH> [--dry-run] [--fee-rate N] [--out-dir <DIR>]]

INPUT
  --from        repeatable. The web wallet's backup ({\"format\":\"kob-backup\",..}), the indexer recovery / export file
                ({\"version\":1|2,\"orders\":[..]}, `kob-executor index export-orders`, the web's indexer export), or an
                indexer order view (GET /v1/orders/{id}) or page of views ({\"items\":[..]}). Orders are deduplicated by
                covenant id; every state a file names is a search candidate, nothing in a file is trusted.
  --node        node wRPC JSON endpoint (18110 mainnet, 18210 testnet-10); `getUtxosByAddresses` needs --utxoindex
  --network     network of the files and the addresses (default testnet-10)
  --maker       only this maker's orders (64-hex x-only pubkey or kaspa address); others are reported `other_maker`
  --amount-left the amount the order has left after partial fills, as an indexer view shows it (`amount_left`):
                base units, or a decimal amount of whole tokens with at most the token's decimals (`1.5` with 8
                decimals = 150000000; a plain integer is base units). Repeatable; `<covenant id>=<amount>` names one
                order, a bare amount applies to every order of the run. It is tried right after the states the files
                name. Needed after a partial fill the fallback below does not reach.

SEARCH
  For each order: its template (pinned by this build), the candidate states and the P2SH address of each; the order is `live` when the node holds an unspent output at that
  exact script carrying the order's covenant id. Candidates, in this order: the last proven state and the given state;
  each with `amountLeft` = the --amount-left hints; each with `amountLeft` = original - k x minFill for k = 1, 2, ...
  (at most 200 candidates in all). A partial fill splices the new `amountLeft` into the script, and any amount of at
  least `minFill` may have been filled, so this search is NOT exhaustive: an order partly filled by another amount is
  reported `not_found` unless you pass its amount with --amount-left. Ask-side kinds: the
  custody (`amountLeft` base units owned by the covenant id) is rebuilt and looked up the same way: `verified`,
  `missing`, or `unknown` (KCC-20 without an extension commitment in the file). `not_found`: spent, or a state the
  search did not reach (see above; kinds with a stop, an armed band or repeat fields also change other fields).
  `unsupported`: the template is not pinned by this build, or the state does not decode. An order of a template this
  build does not pin is ended by its maker with a raw transaction spending the order's own cancel entry.

OUTPUT
  --out         write the JSON report here (default: stdout); a summary goes to stderr. Each live order carries
                `amount_left` (base units; a bid: its remaining buying power), `scale`, `min_fill` and `found_by`
                (`known state`, `--amount-left` or `fallback search`).

CANCEL
  --cancel      build the maker's cancel of every live order of the key's maker (custody verified when it has one): the
                custody and the order's KAS back to the maker, one transaction per order. Strays are not swept (the node cannot tell them apart;
                sweep them with `kob order sweep|cancel` from an indexer view first).
                The fee comes from the order's released KAS; if that is not enough, the smallest sufficient plain KAS UTXO
                of the maker on the node is added.
  --key         the maker's signing secret (64 hex; env:NAME and file:PATH keep it out of shell history); a key that is
                not the order's maker is refused
  --dry-run     build, sign and engine-validate, write, never submit
  --fee-rate    sompi per gram (default 100)
  --out-dir     write <covenant id>.cancel.json (the signed transaction and its submitRequest) here
";

/// Most candidate states per order.
pub const MAX_CANDIDATES: usize = 200;

/// Parsed arguments.
#[derive(Debug, Clone)]
pub struct Args {
    /// Input files.
    pub from: Vec<PathBuf>,
    /// Node URL.
    pub node: String,
    /// Network.
    pub network: String,
    /// Maker filter.
    pub maker: Option<[u8; 32]>,
    /// `--amount-left` hints: the covenant id they are for (`None`: every order) and the amount as typed.
    pub amount_left: Vec<(Option<[u8; 32]>, String)>,
    /// Report path.
    pub out: Option<PathBuf>,
    /// Cancel live orders.
    pub cancel: bool,
    /// Key source.
    pub key: Option<String>,
    /// Never submit.
    pub dry_run: bool,
    /// Fee rate.
    pub fee_rate: u64,
    /// Where the signed cancels go.
    pub out_dir: Option<PathBuf>,
}

/// One `--amount-left` value: `<amount>` or `<covenant id>=<amount>`. The amount is checked against the order's scale
/// when the order is searched (a decimal amount needs the token's decimals); here only its shape.
fn parse_hint(v: &str) -> Result<(Option<[u8; 32]>, String), String> {
    let (cov, amount) = match v.split_once('=') {
        Some((c, a)) => (Some(hex32(c).map_err(|e| format!("--amount-left covenant id: {e}"))?), a),
        None => (None, v),
    };
    let shape_ok = !amount.is_empty()
        && amount.matches('.').count() <= 1
        && amount != "."
        && amount.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !shape_ok {
        return Err(format!("--amount-left must be base units or a decimal amount, got `{amount}`"));
    }
    Ok((cov, amount.to_string()))
}

/// Parse the arguments after `kob recover`.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        from: vec![],
        node: String::new(),
        network: "testnet-10".into(),
        maker: None,
        amount_left: vec![],
        out: None,
        cancel: false,
        key: None,
        dry_run: false,
        fee_rate: MIN_FEE_RATE,
        out_dir: None,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut val = |what: &str| -> Result<String, String> { it.next().cloned().ok_or_else(|| format!("{what} needs a value")) };
        match flag.as_str() {
            "--from" => a.from.push(PathBuf::from(val("--from")?)),
            "--node" => a.node = val("--node")?,
            "--network" => a.network = val("--network")?,
            "--maker" => a.maker = Some(crate::token::parse_pubkey(&val("--maker")?)?),
            "--out" => a.out = Some(PathBuf::from(val("--out")?)),
            "--amount-left" => a.amount_left.push(parse_hint(&val("--amount-left")?)?),
            "--cancel" => a.cancel = true,
            "--key" => a.key = Some(val("--key")?),
            "--dry-run" => a.dry_run = true,
            "--fee-rate" => {
                let v = val("--fee-rate")?;
                a.fee_rate = v.parse().map_err(|_| format!("--fee-rate must be a positive integer, got `{v}`"))?;
            }
            "--out-dir" => a.out_dir = Some(PathBuf::from(val("--out-dir")?)),
            other => return Err(format!("unknown argument `{other}` (see `kob recover --help`)")),
        }
    }
    if a.from.is_empty() {
        return Err("at least one --from file is required".into());
    }
    if a.node.is_empty() {
        return Err("--node is required (every order is verified on the node)".into());
    }
    crate::common::prefix_for_network(&a.network)?;
    if a.cancel && a.key.is_none() {
        return Err("--cancel needs --key".into());
    }
    if !a.cancel && (a.key.is_some() || a.dry_run || a.out_dir.is_some()) {
        return Err("--key, --dry-run and --out-dir belong to --cancel".into());
    }
    if a.fee_rate < MIN_FEE_RATE {
        return Err(format!("--fee-rate must be at least {MIN_FEE_RATE}"));
    }
    Ok(a)
}

/// A candidate that is a state the files name.
pub const VIA_KNOWN: &str = "known state";
/// A candidate with the amount left the maker gave with `--amount-left`.
pub const VIA_HINT: &str = "--amount-left";
/// A candidate from the bounded fallback `amountLeft - k x minFill` (not exhaustive).
pub const VIA_FALLBACK: &str = "fallback search";

/// One candidate state and its script.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The state.
    pub state: AnyState,
    /// Its P2SH script public key.
    pub spk: ScriptPublicKey,
    /// How the state came to be a candidate (`VIA_*`).
    pub via: &'static str,
}

/// The candidate states of an order, distinct scripts only, at most [`MAX_CANDIDATES`], in this order:
///
/// 1. the known states (latest first);
/// 2. each known state with `amountLeft` = each of the maker's `hints` (base units);
/// 3. each known state with `amountLeft = original - k x minFill`, k = 1, 2, ... while positive. A partial fill splices
///    the new `amountLeft` into the script and any amount of at least `minFill` may have been filled, so this is a
///    bounded guess and NOT exhaustive.
pub fn candidates(tpl: TemplateId, known: &[AnyState], hints: &[i64]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = vec![];
    let push = |s: AnyState, via: &'static str, out: &mut Vec<Candidate>| {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        if let Ok(spk) = order_spk(tpl, &s) {
            if !out.iter().any(|c| c.spk == spk) {
                out.push(Candidate { state: s, spk, via });
            }
        }
    };
    for s in known {
        push(s.clone(), VIA_KNOWN, &mut out);
    }
    for s in known {
        for h in hints {
            if let Some(c) = with_amount_left(s, *h) {
                push(c, VIA_HINT, &mut out);
            }
        }
    }
    for s in known {
        if let Some(n) = s.amount_left() {
            let m = s.min_fill();
            if m > 0 {
                for k in 1..=MAX_CANDIDATES as i64 {
                    let Some(left) = k.checked_mul(m).and_then(|d| n.checked_sub(d)).filter(|a| *a >= 1) else { break };
                    if let Some(c) = with_amount_left(s, left) {
                        push(c, VIA_FALLBACK, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// Custody of a live order.
#[derive(Debug, Clone)]
pub enum Custody {
    /// The kind (or an empty entry) holds no custody.
    None,
    /// Found on the node.
    Verified(NodeUtxo, TokenState),
    /// Not on the node.
    Missing(TokenState),
    /// Cannot be rebuilt.
    Unknown(String),
}

/// A custody a live state implies: its token state, its script and the token covenant id.
pub type CustodyTarget = (TokenState, ScriptPublicKey, [u8; 32]);

/// A custody part of a state: (token covenant id, amount, token program template hash, extension commitment).
type CustodyPart = ([u8; 32], i64, [u8; 32], Option<[u8; 32]>);

/// The custody state and script a live state implies (`Ok(None)`: none needed; a pair order: its first custody).
#[cfg(test)]
pub fn custody_target(
    state: &AnyState,
    covenant_id: [u8; 32],
    ext: Option<[u8; 32]>,
) -> Result<Option<(TokenState, ScriptPublicKey)>, String> {
    Ok(custody_targets(state, covenant_id, ext)?.into_iter().next().map(|(st, spk, _)| (st, spk)))
}

/// Every custody a live state holds, in record order, with its state, script and token covenant id: a KAS kind its one
/// custody, a pair order each of `AnyState::custodies` under the program of that token (a sell-first `KobIfdPair`: its A
/// custody, then its B prefund). `ext` is the first custody's extension commitment; the second one (the B prefund) carries
/// the entry's `bExt`.
pub fn custody_targets(state: &AnyState, covenant_id: [u8; 32], ext: Option<[u8; 32]>) -> Result<Vec<CustodyTarget>, String> {
    let parts: Vec<CustodyPart> = match state {
        s if s.is_pair() => {
            let t = s.pair_tokens().ok_or("no pair tokens")?;
            let b_ext = match s {
                AnyState::KobIfdPair(i) => Some(i.b_ext),
                _ => None,
            };
            s.custodies()
                .into_iter()
                .enumerate()
                .map(|(k, (tok, amount))| {
                    let tpl = if tok == t.a.cov_id { t.a.tpl_hash } else { t.b.tpl_hash };
                    (tok, amount, tpl, if k == 0 { ext } else { b_ext })
                })
                .collect()
        }
        _ => match state.custody_amount() {
            Some(a) if a > 0 => vec![(state.token_cov_id(), a, state.token_tpl_hash().ok_or("no token program")?, ext)],
            _ => vec![],
        },
    };
    let mut out = vec![];
    for (tok, amount, hash, ext) in parts {
        if amount <= 0 {
            continue;
        }
        let tt = token_template_by_hash(&hash).ok_or_else(|| format!("token program {} is not supported", to_hex(&hash)))?;
        let ext = match tt.family {
            Family::Kron => [0; 32],
            Family::Kcc20 => ext.ok_or("unknown (no extension commitment)")?,
        };
        let st = TokenState::custody(tt.family, amount, covenant_id, ext);
        let spk = st.spk_with(tt);
        out.push((st, spk, tok));
    }
    Ok(out)
}

/// Status of one order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Unspent on the node.
    Live,
    /// Not found.
    NotFound,
    /// Template or state not usable.
    Unsupported,
    /// Filtered out by `--maker`.
    OtherMaker,
}

impl Status {
    /// Report label.
    pub fn label(&self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::NotFound => "not_found",
            Status::Unsupported => "unsupported",
            Status::OtherMaker => "other_maker",
        }
    }
}

/// What the search found for one order.
#[derive(Debug, Clone)]
pub struct Found {
    /// The merged entry.
    pub entry: Entry,
    /// The template (when this build pins it).
    pub tpl: Option<TemplateId>,
    /// Status.
    pub status: Status,
    /// Explanation.
    pub note: String,
    /// The decoded known state (first that decodes).
    pub known: Option<AnyState>,
    /// Candidates searched.
    pub searched: usize,
    /// Live UTXO and its state.
    pub live: Option<(NodeUtxo, AnyState)>,
    /// How the live state was found (`VIA_*`).
    pub via: Option<&'static str>,
    /// Custody of a live order (a pair order: its first custody).
    pub custody: Custody,
    /// A pair order's second custody (a sell-first `KobIfdPair`'s B prefund); `Custody::None` for every other order.
    pub prefund: Custody,
    /// The cancel outcome (`--cancel`).
    pub cancel: Option<Value>,
}

/// The `--amount-left` hints that apply to the order `covenant_id`, parsed with the order's `scale` (a decimal amount
/// needs it); the ones that do not parse are returned as notes.
fn hints_for(hints: &[(Option<[u8; 32]>, String)], covenant_id: [u8; 32], scale: Option<i64>) -> (Vec<i64>, String) {
    let mut amounts = vec![];
    let mut notes = String::new();
    for (_, text) in hints.iter().filter(|(c, _)| c.is_none_or(|c| c == covenant_id)) {
        match parse_amount(text, scale) {
            Ok(n) => amounts.push(n),
            Err(e) => notes += &format!("--amount-left ignored: {e}; "),
        }
    }
    (amounts, notes)
}

fn prepare(e: Entry, maker: Option<[u8; 32]>, hints: &[(Option<[u8; 32]>, String)]) -> (Found, Vec<Candidate>) {
    let mut f = Found {
        entry: e.clone(),
        tpl: order_template(&e.template_hash),
        status: Status::Unsupported,
        note: String::new(),
        known: None,
        searched: 0,
        live: None,
        via: None,
        custody: Custody::None,
        prefund: Custody::None,
        cancel: None,
    };
    let Some(tpl) = f.tpl else {
        f.note = "template is not pinned by this build".into();
        return (f, vec![]);
    };
    let mut known: Vec<AnyState> = vec![];
    let mut errors = vec![];
    for s in &e.spans {
        match decode_state(tpl, s) {
            Ok(st) => known.push(st),
            Err(err) => errors.push(err),
        }
    }
    let Some(first) = known.first().cloned() else {
        f.note = if errors.is_empty() {
            "the file names no proven state of this order".into()
        } else {
            format!("state does not decode: {}", errors.join("; "))
        };
        return (f, vec![]);
    };
    // every known state must be the same order: kind, maker and token
    known.retain(|s| s.template_id() == first.template_id() && s.maker() == first.maker() && s.token_cov_id() == first.token_cov_id());
    f.known = Some(first.clone());
    if let Some(k) = &e.claimed_kind {
        if k != first.template_id().name() {
            f.note = format!("the file says {k}, the template says {}; ", first.template_id().name());
        }
    }
    if let Some(m) = e.claimed_maker.filter(|m| *m != first.maker()) {
        f.note += &format!("the file names maker {}, the state {}; ", to_hex(&m), to_hex(&first.maker()));
    }
    if maker.is_some_and(|m| m != first.maker()) {
        f.status = Status::OtherMaker;
        f.note += "another maker's order (--maker)";
        return (f, vec![]);
    }
    let (amounts, notes) = hints_for(hints, e.covenant_id, Some(first.scale()));
    f.note += &notes;
    let c = candidates(tpl, &known, &amounts);
    f.searched = c.len();
    f.status = Status::NotFound;
    (f, c)
}

/// Why nothing was found, and how far the search could see (the wording follows what the kind's script can change).
fn not_found_note(known: Option<&AnyState>, searched: usize) -> String {
    match known {
        Some(k) if script_is_fixed(k) => format!("spent: no unspent output at the order's script ({searched} candidate)"),
        Some(a) if amount_is_the_only_mutable_field(a.template_id()) => format!(
            "spent, or partly filled to an amount the search did not reach ({searched} candidates: the known states, the --amount-left hints, then amountLeft - k x minFill; that search is not exhaustive: pass the order's amount left from an indexer view with --amount-left)"
        ),
        _ => format!(
            "spent, or a state the search did not reach ({searched} candidates over the amount left; a moved stop, an armed band or repeat fields change the script too)"
        ),
    }
}

/// Search every entry on the node (two rounds: order scripts, then the custody scripts of the live ones). `hints` are the
/// `--amount-left` values.
pub fn search(
    entries: Vec<Entry>,
    maker: Option<[u8; 32]>,
    hints: &[(Option<[u8; 32]>, String)],
    node: &str,
    network: &str,
) -> Result<Vec<Found>, String> {
    let prepared: Vec<(Found, Vec<Candidate>)> = entries.into_iter().map(|e| prepare(e, maker, hints)).collect();
    let spks: Vec<ScriptPublicKey> = prepared.iter().flat_map(|(_, c)| c.iter().map(|c| c.spk.clone())).collect();
    let at = lookup(node, network, &spks)?;
    let mut found = vec![];
    for (mut f, cands) in prepared {
        if f.status == Status::NotFound {
            let hit = cands.iter().find_map(|c| {
                at.get(&spk_bytes(&c.spk))
                    .and_then(|v| v.iter().find(|u| u.covenant_id == Some(f.entry.covenant_id)))
                    .map(|u| (u.clone(), c.state.clone(), c.via))
            });
            match hit {
                Some((u, state, via)) => {
                    f.status = Status::Live;
                    f.live = Some((u, state));
                    f.via = Some(via);
                }
                None => f.note += &not_found_note(f.known.as_ref(), f.searched),
            }
        }
        found.push(f);
    }
    // custodies of the live token-holding orders (a pair order: one per token it holds)
    let mut targets: Vec<Vec<CustodyTarget>> = vec![];
    for f in found.iter_mut() {
        let t = match &f.live {
            Some((_, st)) => match custody_targets(st, f.entry.covenant_id, f.entry.ext) {
                Ok(t) => t,
                Err(e) => {
                    f.custody = Custody::Unknown(e);
                    vec![]
                }
            },
            None => vec![],
        };
        targets.push(t);
    }
    let spks: Vec<ScriptPublicKey> = targets.iter().flatten().map(|(_, s, _)| s.clone()).collect();
    let at = if spks.is_empty() { Default::default() } else { lookup(node, network, &spks)? };
    for (f, t) in found.iter_mut().zip(targets) {
        for (k, (st, spk, token)) in t.into_iter().enumerate() {
            let hit = at.get(&spk_bytes(&spk)).and_then(|v| v.iter().find(|u| u.covenant_id == Some(token))).cloned();
            let c = match hit {
                Some(u) => Custody::Verified(u, st),
                None => Custody::Missing(st),
            };
            if k == 0 {
                f.custody = c;
            } else {
                f.prefund = c;
            }
        }
    }
    Ok(found)
}

fn custody_json(c: &Custody) -> Value {
    match c {
        Custody::None => json!({"status": "none"}),
        Custody::Verified(u, st) => json!({
            "status": "verified",
            "outpoint": outpoint_str(&u.txid, u.index),
            "amount": st.amount().to_string(),
            "value": u.amount.to_string(),
        }),
        Custody::Missing(st) => json!({"status": "missing", "amount": st.amount().to_string()}),
        Custody::Unknown(why) => json!({"status": "unknown", "reason": why}),
    }
}

/// The report entry of one order.
pub fn order_json(f: &Found) -> Value {
    let state = f.live.as_ref().map(|(_, s)| s).or(f.known.as_ref());
    let mut v = json!({
        "covenant_id": to_hex(&f.entry.covenant_id),
        "kind": state.map(|s| s.template_id().name()),
        "template": f.tpl.map(|_| "pinned"),
        "template_hash": to_hex(&f.entry.template_hash),
        "maker": state.map(|s| to_hex(&s.maker())),
        "status": f.status.label(),
        "note": f.note,
        "source": f.entry.source,
        "candidates": f.searched,
    });
    if let Some((u, s)) = &f.live {
        v["outpoint"] = json!(outpoint_str(&u.txid, u.index));
        v["value"] = json!(u.amount.to_string());
        v["utxo_daa"] = json!(u.daa.to_string());
        v["state"] = json!(f.tpl.and_then(|t| encode_state(t, s).ok()).map(|b| to_hex(&b)));
        // base-unit quantities as strings, like every other 64-bit value of the report
        v["amount_left"] = json!(amount_left_in(s, u.amount).map(|a| a.to_string()));
        v["scale"] = json!(s.scale().to_string());
        v["min_fill"] = json!(s.min_fill().to_string());
        v["found_by"] = json!(f.via);
        v["custody"] = custody_json(&f.custody);
        if !matches!(f.prefund, Custody::None) {
            v["prefund"] = custody_json(&f.prefund);
        }
    }
    if let Some(c) = &f.cancel {
        v["cancel"] = c.clone();
    }
    v
}

/// The whole report.
pub fn report_json(a: &Args, found: &[Found], rejected: &[Rejected]) -> Value {
    let count = |s: Status| found.iter().filter(|f| f.status == s).count();
    json!({
        "network": a.network,
        "node": a.node,
        "orders": found.iter().map(order_json).collect::<Vec<_>>(),
        "rejected": rejected.iter().map(|r| json!({"source": r.source, "reason": r.reason})).collect::<Vec<_>>(),
        "summary": {
            "live": count(Status::Live),
            "not_found": count(Status::NotFound),
            "unsupported": count(Status::Unsupported),
            "other_maker": count(Status::OtherMaker),
            "rejected": rejected.len(),
        },
    })
}

/// Build, sign, validate, write and (unless dry run) submit the maker's cancel of one live order.
fn cancel_one(f: &Found, signer: &Signer, a: &Args) -> Result<Value, String> {
    let (u, state) = f.live.as_ref().ok_or("not live")?;
    let maker = state.maker();
    if maker != signer.pubkey {
        return Err(format!("the key is not this order's maker ({})", to_hex(&maker)));
    }
    let custody = match &f.custody {
        Custody::None => None,
        Custody::Verified(c, st) => Some(TokenUtxo { utxo: utxo_of(c), state: st.clone() }),
        Custody::Missing(_) => return Err("custody not found on the node: not cancelled".into()),
        Custody::Unknown(why) => return Err(format!("custody {why}: not cancelled")),
    };
    // a sell-first pair entry's B prefund (its second custody)
    let prefund = match &f.prefund {
        Custody::None => None,
        Custody::Verified(c, st) => Some(TokenUtxo { utxo: utxo_of(c), state: st.clone() }),
        Custody::Missing(_) => return Err("the B prefund custody was not found on the node: not cancelled".into()),
        Custody::Unknown(why) => return Err(format!("prefund custody {why}: not cancelled")),
    };
    let utxo = utxo_of(u);
    let fee = FeeOptions::rate(a.fee_rate);
    let (built, funding) = build_funded(true, &a.node, &a.network, &maker, |funding| {
        build(&Action::CancelOrder(CancelOrder {
            order: OrderUtxo { utxo: utxo.clone(), state: state.clone() },
            custody: custody.clone(),
            prefund: prefund.clone(),
            strays: vec![],
            foreign: vec![],
            tokens: vec![],
            funding,
            change: None,
            replace: None,
            lock_time: 0,
            records: vec![],
            fee: fee.clone(),
        }))
    })?;
    let cov = to_hex(&f.entry.covenant_id);
    let info = json!({"action": "cancel", "covenant_id": cov, "kind": state.template_id().name()});
    let sent = sign_validate_send(&built, signer, a.out_dir.as_deref(), &format!("{cov}.cancel.json"), info, a.dry_run, &a.node)?;
    Ok(json!({
        "status": if sent.submitted.is_some() { "submitted" } else { "signed" },
        "txid": sent.txid,
        "fee": sent.fee.to_string(),
        "funding": match funding {
            Funding::Own => json!("order"),
            Funding::Key(k) => json!(outpoint_str(&k.utxo.transaction_id, k.utxo.index)),
        },
        "file": sent.file.map(|p| p.display().to_string()),
        "engine_validated": true,
    }))
}

/// Run the search (and the cancels); returns the report and whether every attempted cancel succeeded.
pub fn execute(a: &Args) -> Result<(Value, bool), String> {
    let mut entries = vec![];
    let mut rejected = vec![];
    for path in &a.from {
        let p = inputs::read_file(path, &a.network)?;
        entries.extend(p.entries);
        entries.extend(p.views.iter().map(|v| v.entry()));
        rejected.extend(p.rejected);
    }
    let (entries, notes) = inputs::merge(entries);
    rejected.extend(notes);
    let mut found = search(entries, a.maker, &a.amount_left, &a.node, &a.network)?;
    let mut ok = true;
    if a.cancel {
        let signer = Signer::load(a.key.as_deref().expect("checked"))?;
        for f in found.iter_mut() {
            if f.status != Status::Live {
                continue;
            }
            if f.live.as_ref().is_some_and(|(_, s)| s.maker() != signer.pubkey) {
                f.cancel = Some(json!({"status": "skipped", "reason": "another maker's order (not the key's)"}));
                continue;
            }
            f.cancel = Some(match cancel_one(f, &signer, a) {
                Ok(v) => v,
                Err(e) => {
                    ok = false;
                    json!({"status": "failed", "reason": e})
                }
            });
        }
    }
    Ok((report_json(a, &found, &rejected), ok))
}

fn summary(report: &Value, a: &Args) {
    for o in report["orders"].as_array().into_iter().flatten() {
        let mut line = format!(
            "{:<11} {} {} ({})",
            o["status"].as_str().unwrap_or(""),
            o["covenant_id"].as_str().unwrap_or(""),
            o["kind"].as_str().unwrap_or("?"),
            o["template"].as_str().unwrap_or("template not pinned by this build")
        );
        if let Some(op) = o["outpoint"].as_str() {
            let v = o["value"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
            line += &format!(" at {op}, {} KAS, custody {}", kas(v), o["custody"]["status"].as_str().unwrap_or("?"));
        }
        if let Some(n) = o["note"].as_str().filter(|n| !n.is_empty()) {
            line += &format!(": {n}");
        }
        eprintln!("{line}");
        if let Some(c) = o.get("cancel") {
            eprintln!(
                "  cancel {} {}{}",
                c["status"].as_str().unwrap_or(""),
                c["txid"].as_str().unwrap_or(""),
                c["reason"].as_str().map(|r| format!(": {r}")).unwrap_or_default()
            );
        }
    }
    for r in report["rejected"].as_array().into_iter().flatten() {
        eprintln!("rejected    {}: {}", r["source"].as_str().unwrap_or(""), r["reason"].as_str().unwrap_or(""));
    }
    let s = &report["summary"];
    eprintln!(
        "{} live, {} not found, {} unsupported, {} other maker, {} rejected",
        s["live"], s["not_found"], s["unsupported"], s["other_maker"], s["rejected"]
    );
    if a.cancel {
        eprintln!(
            "strays, if any, are not swept: use `kob order sweep` with an indexer view first{}",
            if a.dry_run { "; dry run: nothing submitted" } else { "" }
        );
    }
}

/// Run `kob recover`; returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return 0;
    }
    let a = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match execute(&a) {
        Ok((report, ok)) => {
            summary(&report, &a);
            match &a.out {
                Some(p) => {
                    if let Err(e) = write_json(p, &report) {
                        eprintln!("error: {e}");
                        return 1;
                    }
                }
                None => println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default()),
            }
            if ok {
                0
            } else {
                1
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
