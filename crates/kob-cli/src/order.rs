//! The sweep and the cancel of one order from an indexer order view: what `kob sweep` and
//! `kob cancel` build, from the indexer by covenant id or from a saved view file (`--view`).
//!
//! The view names the current UTXO of the order, its custody and its strays, with their proven
//! states. Every one of them is looked up on the node, by exact script and covenant id, before
//! anything is built. A view whose order UTXO the node does not hold is refused as stale.

use std::collections::BTreeMap;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{token_template, token_template_by_hash, TemplateId};
use kob_protocol::build::{build, Action, CancelOrder, ForeignStrays, SweepOrder, TokenRef};
use kob_protocol::json::to_hex;
use kob_protocol::state::{AnyState, TokenState};
use kob_protocol::tx::{spk_to_string, FeeOptions, OrderUtxo, TokenUtxo, Utxo};
use serde_json::{json, Value};

use crate::common::{b_token, build_funded, kas, lookup, order_script, outpoint_str, spk_bytes, utxo_of, Funding};
use crate::inputs::{TokenView, View};
use crate::recover::custody_targets;

/// The operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Sweep in place.
    Sweep,
    /// Cancel.
    Cancel,
}

/// What `prepare` needs besides the view: the operation, the node and network, the fee.
#[derive(Debug, Clone)]
pub struct Opts {
    /// Operation.
    pub op: Op,
    /// Node URL.
    pub node: String,
    /// Network.
    pub network: String,
    /// The fee options of the command.
    pub fee: FeeOptions,
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
                "{} units of {} ({} UTXO{}, {} KAS)",
                t["units"].as_str().unwrap_or("?"),
                t["token"].as_str().unwrap_or(""),
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
    /// How the fee is paid (read by the tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub funding: Funding,
    /// The units and carriers per token that return to the maker (read by the tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub returned: Vec<Value>,
    /// The strays that the transaction does not take.
    pub left: Vec<Value>,
}

/// Build the sweep or cancel of one view.
///
/// `Err(outcome)` means skipped or refused, and nothing is built.
pub(crate) fn prepare(v: &View, a: &Opts, maker_key: [u8; 32]) -> Result<Result<Prepared, Outcome>, String> {
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
        return Ok(Err(skip("an order under an older contract version is only cancelled (kob cancel)")));
    }
    if state.template_id() != script.kind() {
        return Ok(Err(refuse(format!(
            "the view's state is a {}, its template a {}",
            state.template_id().name(),
            script.kind().name()
        ))));
    }
    let order_spk = script.spk(state)?;
    // A cancel reads an unbound curve under its custody's token (the output of its own genesis).
    // The order's script and the builder keep the state as it is on chain.
    let named = match a.op {
        Op::Cancel => match crate::recover::named_state(state, Some(cur_txid), v.custody.as_ref()) {
            Ok(s) => s,
            Err(e) => return Ok(Err(refuse(e))),
        },
        Op::Sweep => state.clone(),
    };
    let own_program = state
        .token_tpl_hash()
        .and_then(|h| token_template_by_hash(&h))
        .map(|t| t.id)
        .ok_or("the order's token program is not supported")?;
    let b_token = b_token(&named)?;
    let own = named.token_cov_id();
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
    let targets = if a.op == Op::Cancel { custody_targets(&named, cov, v.ext)? } else { vec![] };
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
    // A mode-1 order's bought-custody goes back to the maker with a cancel (the builder needs it).
    let bought_target = if a.op == Op::Cancel { crate::trade::data::bought_target(state, cov)? } else { None };
    if let Some((st, _, token)) = &bought_target {
        match &v.bought {
            Some(b) if b.token == *token && b.amount == st.amount() => {}
            _ => return Ok(Err(refuse("the view names no bought-custody UTXO of the order's `bought` base units"))),
        }
    }
    let mut spks = vec![order_spk.clone()];
    spks.extend(cands.iter().map(|c| c.3.clone()));
    spks.extend(custody_views.iter().map(|c| c.2.clone()));
    spks.extend(bought_target.iter().map(|t| t.1.clone()));
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
    let bought: Option<TokenUtxo> = match (&bought_target, &v.bought) {
        (Some((st, spk, _)), Some(bv)) => match on_node(&at, spk, bv) {
            Some(u) => Some(TokenUtxo { utxo: u, state: st.clone() }),
            None => return Ok(Err(refuse("the view's bought-custody UTXO is not on the node (stale view)"))),
        },
        _ => None,
    };
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
        for c in custody.iter().chain(prefund.iter()).chain(bought.iter()).filter(|c| c.utxo.covenant_id == Some(t)) {
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
    let fee = a.fee.clone();
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
                strays: bought.iter().cloned().chain(strays.iter().cloned()).collect(),
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
    let swept: Vec<&TokenUtxo> = bought.iter().chain(strays.iter()).chain(foreign.iter().flat_map(|g| g.utxos.iter())).collect();
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
