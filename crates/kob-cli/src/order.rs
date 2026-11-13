//! `kob order sweep` and `kob order cancel`: the sweep in place and the cancel of the maker.
//!
//! Both work from an indexer order view. The view names the current UTXO of the order, its custody
//! and its strays, with their proven states. Every one of them is looked up on the node, by exact
//! script and covenant id, before anything is built. A view whose order UTXO the node does not hold
//! is refused as stale.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::build::{build, Action, CancelOrder, ForeignStrays, SweepOrder, TokenRef};
use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{spk_to_string, FeeMode, FeeOptions, OrderUtxo, TokenUtxo, Utxo, MIN_FEE_RATE};
use serde_json::{json, Value};

use crate::common::{
    b_token, build_funded, kas, lookup, order_script, outpoint_str, sign_validate_send, spk_bytes, utxo_of, write_json, Funding,
    Signer,
};
use crate::inputs::{self, TokenView, View};
use crate::recover::custody_targets;
use crate::token::{default_key, key_option, KeySource};

/// Usage text of `kob order`.
pub const USAGE: &str = "\
kob order: the maker's stray sweep in place and the maker's cancel, from an indexer order view

USAGE
  kob order sweep  --view <FILE>... (--key-file <PATH> | --key-env <NAME>) --node ws://HOST:PORT
                   [--network NET] [--yes | --dry-run] [--fee-rate N] [--priority-fee] [--out-dir <DIR>] [--out <report.json>]
  kob order cancel --view <FILE>... (--key-file <PATH> | --key-env <NAME>) --node ws://HOST:PORT [same options]

INPUT
  --view        repeatable: an indexer order view (GET /v1/orders/{id}) or a page of views ({\"items\":[..]}, e.g.
                GET /v1/orders?maker=<pubkey>&status=active). Orders of other makers are skipped. The view must name
                the order's current UTXO and its proven state; the node must hold an unspent output at the order's
                script with its covenant id at that outpoint, else the view is refused as stale. The custody and every
                stray are looked up on the node the same way (a stray the node does not hold is left out).
  --key-file    the maker's signing key: a file holding the 64-hex secret
  --key-env     ... or the environment variable holding it (default $KOB_KEY); the older `--key file:PATH`, `--key env:NAME`
                and `--key HEX` still work and print a warning (a key on the command line is visible to other users)
  --node        the node: a JSON wRPC endpoint (18110 mainnet, 18210 testnet-10), a Borsh one (ws://HOST:17110, wss://.../borsh) or `resolver`; default $KOB_NODE_WRPC, else the public nodes the Kaspa resolvers name (a pair on two hosts that must agree on what a transaction is planned from)
                (a URL with a password or an API key belongs in $KOB_NODE_WRPC: arguments are visible in `ps`)
  --network     network of the addresses: mainnet (default, or the network of a deployment build) or testnet-10
                (--testnet, --netsuffix=10)

SWEEP (every live order of the key's maker that has strays)
  The maker's `cancel` entry continues the order IN PLACE: same covenant id, same script, custody untouched; strays of
  the order's own token (a cross limit: also token B) and foreign strays (tokens of other covenant ids, moved with the
  `program` the view names) go back to the maker, one output per token. Per token at most the program's token inputs
  and one extension commitment (the first stray's) per transaction: the rest is reported, run again after this one
  confirms. A stray without a proven state (or a foreign one without a program) is skipped. The order's 90-day idle
  window restarts with the sweep.
  Fee: a plain ask (KobAsk / KobAskKron) pays from its carrier; every other kind adds the smallest sufficient plain KAS
  UTXO of the maker from the node.

CANCEL (every order given)
  The maker's cancel: the custody (the view's custody UTXO), the strays of the order's tokens (within the program's
  token inputs) and foreign strays back to the maker with the order's KAS; strays left behind are reported. Fee from
  the order's released KAS, else the smallest sufficient plain KAS UTXO of the maker.

  An order under a RETIRED template of this build's cancel-only table (an older contract version) is cancelled the same
  way, through that template's own cancel entry (nothing else is built for it: `kob order sweep` skips it). An order of
  any other template this build does not pin is skipped (its maker ends it with the KOB release it was placed with).

OUTPUT
  --yes, -y     sign and submit without asking. Without it, each order is described and a terminal asks (type yes);
                without a terminal the order is refused (status `refused`, exit 1) and nothing is signed.
  --dry-run     build, sign and engine-validate, write, never submit (excludes --yes)
  --fee-rate    sompi per gram (default 100)
  --priority-fee  also pay for the storage mass (KIP-9), as the web app's \"Pay the priority fee\": a higher fee, worth it only when blocks are
                contended (charged at --fee-rate on the storage-inclusive mass)
  --out-dir     write <covenant id>.sweep.json / <covenant id>.cancel.json (signed transaction and submitRequest) here
  --out         write the JSON report here (default: stdout); one line per order goes to stderr
";

/// The operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Sweep in place.
    Sweep,
    /// Cancel.
    Cancel,
}

/// Parsed arguments.
#[derive(Debug, Clone)]
pub struct Args {
    /// Operation.
    pub op: Op,
    /// View files.
    pub views: Vec<PathBuf>,
    /// Key source.
    pub key: Option<KeySource>,
    /// Node URL.
    pub node: String,
    /// Network.
    pub network: String,
    /// Never submit.
    pub dry_run: bool,
    /// Submit without asking.
    pub yes: bool,
    /// Fee rate.
    pub fee_rate: u64,
    /// The storage-inclusive priority fee instead of the relay floor.
    pub priority_fee: bool,
    /// Where the signed transactions go.
    pub out_dir: Option<PathBuf>,
    /// Report path.
    pub out: Option<PathBuf>,
}

/// Parse the arguments after `kob order sweep|cancel`.
pub fn parse_args(op: Op, args: &[String]) -> Result<Args, String> {
    parse_args_with(op, args, default_key())
}

/// [`parse_args`] with the key used when none is named (`$KOB_KEY`).
pub fn parse_args_with(op: Op, args: &[String], fallback_key: Option<KeySource>) -> Result<Args, String> {
    let mut a = Args {
        op,
        views: vec![],
        key: None,
        node: String::new(),
        network: String::new(),
        dry_run: false,
        yes: false,
        fee_rate: MIN_FEE_RATE,
        priority_fee: false,
        out_dir: None,
        out: None,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut val = |what: &str| -> Result<String, String> { it.next().cloned().ok_or_else(|| format!("{what} needs a value")) };
        match flag.as_str() {
            "--view" => a.views.push(PathBuf::from(val("--view")?)),
            "--key" | "--key-file" | "--key-env" => a.key = Some(key_option(flag, &val(flag)?, a.key.is_some())?),
            "--node" => a.node = val("--node")?,
            "--network" => a.network = val("--network")?,
            "--dry-run" => a.dry_run = true,
            "--yes" | "-y" => a.yes = true,
            "--fee-rate" => {
                let v = val("--fee-rate")?;
                a.fee_rate = v.parse().map_err(|_| format!("--fee-rate must be a positive integer, got `{v}`"))?;
            }
            "--priority-fee" => a.priority_fee = true,
            "--out-dir" => a.out_dir = Some(PathBuf::from(val("--out-dir")?)),
            "--out" => a.out = Some(PathBuf::from(val("--out")?)),
            other => return Err(format!("unknown argument `{other}` (see `kob order --help`)")),
        }
    }
    if a.yes && a.dry_run {
        return Err("--yes and --dry-run exclude each other".into());
    }
    if a.views.is_empty() {
        return Err("at least one --view file is required".into());
    }
    if a.key.is_none() {
        a.key = fallback_key;
    }
    if a.key.is_none() {
        return Err("a key is required: give --key-file PATH or --key-env NAME, or set the environment variable KOB_KEY".into());
    }
    a.network = crate::common::resolve_network(&a.network)?;
    a.node = crate::wrpc::node_for(Some(std::mem::take(&mut a.node)), &a.network);
    if a.fee_rate < MIN_FEE_RATE {
        return Err(format!("--fee-rate must be at least {MIN_FEE_RATE}"));
    }
    Ok(a)
}

/// A stray that the node confirmed, with its program.
#[derive(Debug, Clone)]
struct Stray {
    utxo: TokenUtxo,
    program: TemplateId,
}

/// A stray or custody of a view, with the script that its proven state implies under `program`.
pub(crate) fn stray_script(t: &TokenView, program: TemplateId) -> Result<(TokenState, ScriptPublicKey), String> {
    let st = t.state.clone().ok_or("no proven state in the view")?;
    let tt = token_template(program);
    if st.family() != tt.family {
        return Err(format!("its state is not of the {} family", program.name()));
    }
    Ok((st.clone(), st.spk_with(tt)))
}

/// The UTXO of a token entry of a view, if the node holds it at `spk` with its covenant id.
pub(crate) fn on_node(at: &BTreeMap<Vec<u8>, Vec<crate::wrpc::NodeUtxo>>, spk: &ScriptPublicKey, t: &TokenView) -> Option<Utxo> {
    at.get(&spk_bytes(spk))
        .and_then(|v| v.iter().find(|u| u.txid == t.txid && u.index == t.index && u.covenant_id == Some(t.token)))
        .map(utxo_of)
}

/// Units and carriers of each token in a set of token UTXOs.
fn tally(utxos: &[&TokenUtxo]) -> Vec<Value> {
    let mut by: BTreeMap<[u8; 32], (i64, u64, usize)> = BTreeMap::new();
    for u in utxos {
        let e = by.entry(u.utxo.covenant_id.unwrap_or_default()).or_default();
        e.0 += u.state.amount();
        e.1 += u.utxo.amount;
        e.2 += 1;
    }
    by.into_iter()
        .map(|(t, (units, value, n))| json!({"token": to_hex(&t), "units": units.to_string(), "utxos": n, "kas": value.to_string()}))
        .collect()
}

fn describe(list: &[Value]) -> String {
    if list.is_empty() {
        return "no tokens".into();
    }
    list.iter()
        .map(|t| {
            format!(
                "{} units of {}.. ({} UTXO{}, {} KAS)",
                t["units"].as_str().unwrap_or("?"),
                &t["token"].as_str().unwrap_or("")[..16.min(t["token"].as_str().unwrap_or("").len())],
                t["utxos"],
                if t["utxos"] == 1 { "" } else { "s" },
                kas(t["kas"].as_str().and_then(|k| k.parse().ok()).unwrap_or(0))
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The outcome of one order.
pub(crate) struct Outcome {
    /// The status: `skipped`, `refused`, `failed`, `signed` or `submitted`.
    pub(crate) status: &'static str,
    /// The JSON entry of the report.
    pub(crate) json: Value,
}

fn skip(reason: impl Into<String>) -> Outcome {
    Outcome { status: "skipped", json: json!({"status": "skipped", "reason": reason.into()}) }
}

fn refuse(reason: impl Into<String>) -> Outcome {
    Outcome { status: "refused", json: json!({"status": "refused", "reason": reason.into()}) }
}

fn process(v: &View, a: &Args, signer: &Signer) -> Outcome {
    match try_process(v, a, signer) {
        Ok(o) => o,
        Err(e) => Outcome { status: "failed", json: json!({"status": "failed", "reason": e}) },
    }
}

/// A built, unsigned sweep or cancel of one order, ready to show and sign.
pub(crate) struct Prepared {
    /// The transaction.
    pub built: kob_protocol::tx::BuiltTx,
    /// The file name of the signed transaction.
    pub name: String,
    /// The report info that goes with the transaction.
    pub info: Value,
    /// The text shown to the maker before the confirmation.
    pub disclosure: String,
    /// How the fee is paid.
    pub funding: Funding,
    /// The units and carriers per token that return to the maker.
    pub returned: Vec<Value>,
    /// The strays that the transaction does not take.
    pub left: Vec<Value>,
}

fn try_process(v: &View, a: &Args, signer: &Signer) -> Result<Outcome, String> {
    let p = match prepare(v, a, signer.pubkey)? {
        Ok(p) => p,
        Err(o) => return Ok(o),
    };
    let Prepared { built, name, info, disclosure, funding, returned, left } = p;
    if !a.dry_run {
        // The same rule as every other submitting command: `--yes`, or a terminal asks, else
        // nothing is signed.
        if crate::trade::opts::would_ask(a.yes) {
            eprintln!("{disclosure}");
        }
        let what = format!("the {} of {}", if a.op == Op::Sweep { "sweep" } else { "cancel" }, to_hex(&v.covenant_id));
        if let Err(e) = crate::trade::opts::confirm_with(a.yes, &a.network, &what) {
            return Ok(refuse(e));
        }
    }
    let sent = sign_validate_send(&built, signer, a.out_dir.as_deref(), &name, info, a.dry_run, &a.node, &a.network)?;
    Ok(Outcome {
        status: if sent.submitted.is_some() { "submitted" } else { "signed" },
        json: json!({
            "status": if sent.submitted.is_some() { "submitted" } else { "signed" },
            "txid": sent.txid,
            "fee": sent.fee.to_string(),
            "funding": match funding { Funding::Own => json!("order"), Funding::Key(k) => json!(outpoint_str(&k.utxo.transaction_id, k.utxo.index)) },
            "returned": returned,
            "left_behind": left,
            "disclosure": disclosure,
            "file": sent.file.map(|p| p.display().to_string()),
            "engine_validated": true,
        }),
    })
}

/// Build the sweep or cancel of one view.
///
/// `Err(outcome)` means skipped or refused, and nothing is built.
pub(crate) fn prepare(v: &View, a: &Args, maker_key: [u8; 32]) -> Result<Result<Prepared, Outcome>, String> {
    let cov = v.covenant_id;
    let Some(state) = &v.state else { return Ok(Err(skip("the view has no proven state (state_known false)"))) };
    if state.maker() != maker_key {
        return Ok(Err(skip(format!("another maker's order ({})", to_hex(&state.maker())))));
    }
    let Some((cur_txid, cur_index, _)) = v.current else { return Ok(Err(skip("the view names no current UTXO (closed?)"))) };
    let Some(script) = order_script(&v.template_hash) else {
        return Ok(Err(skip("template is neither pinned by this build nor in its cancel-only table")));
    };
    if script.retired().is_some() && a.op == Op::Sweep {
        return Ok(Err(skip("an order under an older contract version is only cancelled (kob order cancel)")));
    }
    if state.template_id() != script.kind() {
        return Ok(Err(refuse(format!(
            "the view's state is a {}, its template a {}",
            state.template_id().name(),
            script.kind().name()
        ))));
    }
    let order_spk = script.spk(state)?;
    let own_program = state
        .token_tpl_hash()
        .and_then(|h| token_template_by_hash(&h))
        .map(|t| t.id)
        .ok_or("the order's token program is not supported")?;
    let b_token = b_token(state)?;
    let own = state.token_cov_id();
    // The scripts to look up: the order, its custody (cancel) and every usable stray.
    let mut left: Vec<Value> = vec![];
    let mut cands: Vec<(TokenView, TemplateId, TokenState, ScriptPublicKey)> = vec![];
    for s in v.strays.iter().filter(|s| !s.spent) {
        let program = if s.token == own {
            Some(own_program)
        } else if b_token.is_some_and(|b| b.0 == s.token) {
            b_token.map(|b| b.1)
        } else {
            s.program
        };
        let why = |r: &str| json!({"outpoint": outpoint_str(&s.txid, s.index), "token": to_hex(&s.token), "reason": r});
        let Some(program) = program else {
            left.push(why("foreign stray without a token program in the view"));
            continue;
        };
        match stray_script(s, program) {
            Ok((st, spk)) if st.owner() == cov && st.is_covenant_owned() => cands.push((s.clone(), program, st, spk)),
            Ok(_) => left.push(why("not owned by the order's covenant id")),
            Err(e) => left.push(why(&e)),
        }
    }
    // The custodies that a cancel returns. That is one for a KAS kind, or one per token for a pair
    // order: the `custody` of the view and, for the B prefund of a sell-first entry,
    // `pair.custodies[1]`. Each is under the program of its token.
    let program_of = |t: [u8; 32]| if t == own { Some(own_program) } else { b_token.filter(|b| b.0 == t).map(|b| b.1) };
    let targets = if a.op == Op::Cancel { custody_targets(state, cov, v.ext)? } else { vec![] };
    let mut custody_views: Vec<(TokenView, TokenState, ScriptPublicKey)> = vec![];
    for (k, (target, target_spk, token)) in targets.into_iter().enumerate() {
        let view = if k == 0 { &v.custody } else { &v.custody_b };
        let Some(c) = view else { return Ok(Err(refuse("the view names no custody UTXO for this token-holding order"))) };
        if c.token != token {
            return Ok(Err(refuse("the view's custody is not of the token the order's state holds")));
        }
        let (st, spk) = match (&c.state, program_of(token)) {
            (Some(_), Some(program)) => stray_script(c, program)?,
            _ => (target.clone(), target_spk),
        };
        if st.amount() != target.amount() || st.owner() != cov {
            return Ok(Err(refuse("the view's custody does not hold the order's amount left (base units)")));
        }
        custody_views.push((c.clone(), st, spk));
    }
    let mut spks = vec![order_spk.clone()];
    spks.extend(cands.iter().map(|c| c.3.clone()));
    spks.extend(custody_views.iter().map(|c| c.2.clone()));
    let at = lookup(&a.node, &a.network, &spks)?;
    let node_order = at
        .get(&spk_bytes(&order_spk))
        .and_then(|us| us.iter().find(|u| u.txid == cur_txid && u.index == cur_index && u.covenant_id == Some(cov)));
    let Some(node_order) = node_order else {
        return Ok(Err(refuse(format!(
            "stale view: the node holds no unspent output at the order's script with its covenant id at {}",
            outpoint_str(&cur_txid, cur_index)
        ))));
    };
    let order_utxo = utxo_of(node_order);
    let mut held: Vec<TokenUtxo> = vec![];
    for (c, st, spk) in &custody_views {
        match on_node(&at, spk, c) {
            Some(u) => held.push(TokenUtxo { utxo: u, state: st.clone() }),
            None => return Ok(Err(refuse("the view's custody UTXO is not on the node (stale view)"))),
        }
    }
    let mut held = held.into_iter();
    let (custody, prefund) = (held.next(), held.next());
    let mut confirmed: Vec<Stray> = vec![];
    for (s, program, st, spk) in cands {
        match on_node(&at, &spk, &s) {
            Some(u) => confirmed.push(Stray { utxo: TokenUtxo { utxo: u, state: st }, program }),
            None => {
                left.push(json!({"outpoint": outpoint_str(&s.txid, s.index), "token": to_hex(&s.token), "reason": "not on the node"}))
            }
        }
    }
    // Within slots: per token, the token inputs of the program. For the own token of the order that
    // is also the cap of the family, less the custody. One extension commitment per token: the
    // custody's, else that of the first stray.
    let mut chosen: Vec<Stray> = vec![];
    let mut tokens: Vec<[u8; 32]> = vec![];
    for s in &confirmed {
        let t = s.utxo.utxo.covenant_id.unwrap_or_default();
        if !tokens.contains(&t) {
            tokens.push(t);
        }
    }
    for t in tokens {
        let group: Vec<&Stray> = confirmed.iter().filter(|s| s.utxo.utxo.covenant_id == Some(t)).collect();
        let program = group[0].program;
        let mut cap = program.token_slots().map(|s| s.0).unwrap_or(0);
        let mut ext = group[0].utxo.state.extension();
        if t == own {
            cap = cap.min(program.family().max_tok_in());
        }
        // A custody of this token takes one of its inputs and sets its extension commitment.
        for c in custody.iter().chain(prefund.iter()).filter(|c| c.utxo.covenant_id == Some(t)) {
            cap = cap.saturating_sub(1);
            ext = c.state.extension();
        }
        for s in group {
            let reason = if s.utxo.state.extension() != ext {
                Some("another extension commitment: run again after this one confirms")
            } else if chosen.iter().filter(|c| c.utxo.utxo.covenant_id == Some(t)).count() >= cap {
                Some("over the program's token inputs: run again after this one confirms")
            } else {
                None
            };
            match reason {
                Some(r) => left.push(json!({"outpoint": outpoint_str(&s.utxo.utxo.transaction_id, s.utxo.utxo.index), "token": to_hex(&t), "reason": r})),
                None => chosen.push(s.clone()),
            }
        }
    }
    let is_own = |s: &Stray| s.utxo.utxo.covenant_id == Some(own) || b_token.is_some_and(|b| s.utxo.utxo.covenant_id == Some(b.0));
    let strays: Vec<TokenUtxo> = chosen.iter().filter(|s| is_own(s)).map(|s| s.utxo.clone()).collect();
    let mut foreign: Vec<ForeignStrays> = vec![];
    for s in chosen.iter().filter(|s| !is_own(s)) {
        let t = s.utxo.utxo.covenant_id.unwrap_or_default();
        match foreign.iter_mut().find(|g| g.token.covenant_id == t) {
            Some(g) => g.utxos.push(s.utxo.clone()),
            None => {
                foreign.push(ForeignStrays { token: TokenRef { covenant_id: t, program: s.program }, utxos: vec![s.utxo.clone()] })
            }
        }
    }
    if a.op == Op::Sweep && strays.is_empty() && foreign.is_empty() {
        let mut o = skip("nothing to sweep");
        o.json["left_behind"] = json!(left);
        return Ok(Err(o));
    }
    let fee = FeeOptions { fee_rate: Some(a.fee_rate), fee_mode: if a.priority_fee { FeeMode::Priority } else { FeeMode::Relay } };
    let maker = state.maker();
    let plain_ask = matches!(state, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
    let (built, funding) = match a.op {
        Op::Sweep => build_funded(plain_ask, &a.node, &a.network, &maker, |funding| {
            build(&Action::SweepOrder(SweepOrder {
                order: OrderUtxo { utxo: order_utxo.clone(), state: state.clone() },
                strays: strays.clone(),
                foreign: foreign.clone(),
                funding,
                change: None,
                token_carrier: None,
                lock_time: 0,
                records: vec![],
                fee: fee.clone(),
            }))
        })?,
        Op::Cancel => build_funded(true, &a.node, &a.network, &maker, |funding| {
            build(&Action::CancelOrder(CancelOrder {
                retired: script.retired(),
                order: OrderUtxo { utxo: order_utxo.clone(), state: state.clone() },
                custody: custody.clone(),
                prefund: prefund.clone(),
                strays: strays.clone(),
                foreign: foreign.clone(),
                tokens: vec![],
                funding,
                change: None,
                replace: None,
                lock_time: 0,
                records: vec![],
                fee: fee.clone(),
            }))
        })?,
    };
    if a.op == Op::Sweep {
        // The sweep continues the order: output 0 is the same script under the same covenant id.
        let o0 = built.tx.outputs.first().ok_or("the sweep has no outputs")?;
        if o0.script_public_key != spk_to_string(&order_spk) || o0.covenant.as_ref().map(|c| c.covenant_id) != Some(cov) {
            return Err("internal: the sweep's output 0 is not the order's continuation".into());
        }
    }
    let swept: Vec<&TokenUtxo> = strays.iter().chain(foreign.iter().flat_map(|g| g.utxos.iter())).collect();
    let returned = tally(&swept);
    let paid_by = match &funding {
        Funding::Own => "the order's own KAS".to_string(),
        Funding::Key(k) => format!("the maker's UTXO {}", outpoint_str(&k.utxo.transaction_id, k.utxo.index)),
    };
    let covh = to_hex(&cov);
    let (name, disclosure) = match a.op {
        Op::Sweep => (
            format!("{covh}.sweep.json"),
            format!(
                "sweep {covh}: returns {} to the maker; the order continues unchanged (same covenant id, same script{}) and its 90-day idle window restarts with this transaction; fee paid by {paid_by}",
                describe(&returned),
                if state.custody_amount().is_some_and(|x| x > 0) { ", custody untouched" } else { "" }
            ),
        ),
        Op::Cancel => (
            format!("{covh}.cancel.json"),
            format!(
                "cancel {covh}{}: ends the order; returns {}{} and the order's KAS to the maker; fee paid by {paid_by}",
                match script {
                    crate::common::OrderScript::Retired(r) => format!(" (older contract version, cancel only: {})", r.label()),
                    crate::common::OrderScript::Pinned(_) => String::new(),
                },
                custody
                    .iter()
                    .chain(prefund.iter())
                    .map(|c| match b_token {
                        // A pair order names the token of each custody. It holds A, B or both.
                        Some(_) => format!("the custody ({} units of {}), ", c.state.amount(), to_hex(&c.utxo.covenant_id.unwrap_or_default())),
                        None => format!("the custody ({} units), ", c.state.amount()),
                    })
                    .collect::<String>(),
                if returned.is_empty() { "no strays".to_string() } else { format!("strays: {}", describe(&returned)) }
            ),
        ),
    };
    let info = json!({"action": if a.op == Op::Sweep { "sweep" } else { "cancel" }, "covenant_id": covh, "kind": state.template_id().name(), "template": script.describe(), "returned": returned});
    Ok(Ok(Prepared { built, name, info, disclosure, funding, returned, left }))
}

/// Run the operation over every view.
///
/// Returns the report and whether nothing failed or was refused.
pub fn execute(a: &Args) -> Result<(Value, bool), String> {
    let signer = Signer::load(a.key.as_ref().expect("checked"))?;
    let mut views = vec![];
    let mut rejected = vec![];
    for p in &a.views {
        let parsed = inputs::read_file(p, &a.network)?;
        if !parsed.entries.is_empty() {
            return Err(format!("{}: not an indexer order view (use `kob recover` for backup and export files)", p.display()));
        }
        views.extend(parsed.views);
        rejected.extend(parsed.rejected);
    }
    let mut ok = rejected.is_empty();
    let mut orders = vec![];
    for v in &views {
        let o = process(v, a, &signer);
        if matches!(o.status, "failed" | "refused") {
            ok = false;
        }
        let mut j = o.json;
        j["covenant_id"] = json!(to_hex(&v.covenant_id));
        j["kind"] = json!(v.state.as_ref().map(|s| s.template_id().name()));
        j["source"] = json!(v.source);
        orders.push(j);
    }
    let report = json!({
        "action": if a.op == Op::Sweep { "sweep" } else { "cancel" },
        "network": a.network,
        "orders": orders,
        "rejected": rejected.iter().map(|r| json!({"source": r.source, "reason": r.reason})).collect::<Vec<_>>(),
    });
    Ok((report, ok))
}

/// Run `kob order sweep|cancel` and return the process exit code.
pub fn run(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[USAGE], true) {
        return code;
    }
    let op = match args.first().map(String::as_str) {
        Some("sweep") => Op::Sweep,
        Some("cancel") => Op::Cancel,
        other => {
            eprintln!("error: unknown `kob order {}`\n", other.unwrap_or(""));
            return crate::help::print_usage_error(&[USAGE]);
        }
    };
    let rest = &args[1..];
    let a = match parse_args(op, rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match execute(&a) {
        Ok((report, ok)) => {
            for o in report["orders"].as_array().into_iter().flatten() {
                let line = o["disclosure"].as_str().map(str::to_owned).unwrap_or_else(|| {
                    format!(
                        "{} {}: {}",
                        o["status"].as_str().unwrap_or(""),
                        o["covenant_id"].as_str().unwrap_or(""),
                        o["reason"].as_str().unwrap_or("")
                    )
                });
                eprintln!("{line}");
                if let Some(t) = o["txid"].as_str() {
                    eprintln!(
                        "  {} {t}{}",
                        o["status"].as_str().unwrap_or(""),
                        if a.dry_run { " (dry run: not submitted)" } else { "" }
                    );
                }
                for l in o["left_behind"].as_array().into_iter().flatten() {
                    eprintln!("  left behind {}: {}", l["outpoint"].as_str().unwrap_or(""), l["reason"].as_str().unwrap_or(""));
                }
            }
            for r in report["rejected"].as_array().into_iter().flatten() {
                eprintln!("rejected {}: {}", r["source"].as_str().unwrap_or(""), r["reason"].as_str().unwrap_or(""));
            }
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
