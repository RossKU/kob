//! Lowering a [`Plan`] to a `kob_protocol::build::Batch` (either token family) and the exact accounting
//! of the built transaction.
//!
//! Nothing here constructs transactions: the batch is a builder request; `kob_protocol` lays out
//! the inputs and outputs, pays every maker exactly its all-in bound and hands the spread, the tips
//! and the keeper tips of the updates to the operator's change output.

use std::collections::BTreeMap;

use kob_protocol::build::{Batch, BatchUpdate, Leg, PairEntryMerge, SellFirstEntry};
use kob_protocol::state::*;
use kob_protocol::tx::{FeeOptions, KeyUtxo, OrderUtxo};

use super::book::{CovId, ListedOrder};
use super::candidate::{PairRole, Side};
use super::planner::Plan;

/// Everything lowering needs besides the plan.
pub struct LowerCtx<'a> {
    pub by_id: &'a BTreeMap<CovId, ListedOrder>,
    /// The operator's P2PK funding (the change output returns it with the spread and tips).
    pub funding: Vec<KeyUtxo>,
    /// Operator key: change and the taker of the batch.
    pub operator: [u8; 32],
    pub fee_rate: u64,
    /// KAS carrier on the operator's token outputs (the reference planner leaves the operator none; kept for builders that do).
    pub token_carrier: u64,
    /// Measured compute budgets (role -> at least this budget) that replace a short table budget. The table is the maximum
    /// measured over every shape the planner builds (`kob-protocol/tests/budget_table.rs`); when the engine validation still
    /// reports `ExceededCommittedScriptUnits` (a shape the generator misses), the engine measures the transaction's inputs
    /// ([`measured_floors`]) and retries once with the budgets they need. Empty: the table.
    pub budget_floor: BTreeMap<String, u16>,
}

/// The typed state of an order UTXO. The KRON kinds carry the states of their KCC-20 counterparts; the builders
/// derive the family from the leg's token program.
fn typed<S>(o: &OrderUtxo<AnyState>, f: impl Fn(&AnyState) -> Option<S>) -> Result<OrderUtxo<S>, String> {
    let base = o.state.clone().into_family(kob_protocol::family::Family::Kcc20);
    Ok(OrderUtxo { utxo: o.utxo.clone(), state: f(&base).ok_or("order kind does not match its candidate")? })
}

/// Builds the `Batch` request of a plan (either family: the builders follow the legs' token programs). Leg `i` is
/// `plan.fills[i]`, so a triggered fill's and an update's `evidence` index is the evidence fill's position in the plan.
pub fn lower_batch(plan: &Plan, cx: &LowerCtx) -> Result<Batch, String> {
    let get = |id: &CovId| cx.by_id.get(id).ok_or_else(|| format!("order {} is no longer listed", kob_protocol::json::to_hex(id)));
    let mut legs = vec![];
    for f in &plan.fills {
        let o = get(&f.cand.id)?;
        let amount = f.amount;
        let evidence = f.evidence;
        let custody = || o.custody.clone().ok_or_else(|| "ask-side order without custody".to_string());
        let leg = match &o.base_state() {
            AnyState::KobAsk(_) => Leg::Ask {
                order: typed(&o.order, |s| if let AnyState::KobAsk(x) = s { Some(x.clone()) } else { None })?,
                custody: custody()?,
                amount,
                t: None,
            },
            AnyState::KobBid(_) => Leg::Bid {
                order: typed(&o.order, |s| if let AnyState::KobBid(x) = s { Some(x.clone()) } else { None })?,
                amount,
                t: None,
            },
            AnyState::KobCondAsk(_) => {
                let merge = match f.cand.merge {
                    Some(e) => {
                        let eo = get(&e)?;
                        Some(typed(&eo.order, |s| if let AnyState::KobIfdBid(x) = s { Some(x.clone()) } else { None })?)
                    }
                    None => None,
                };
                Leg::CondAsk {
                    order: typed(&o.order, |s| if let AnyState::KobCondAsk(x) = s { Some(x.clone()) } else { None })?,
                    custody: custody()?,
                    amount,
                    leg: f.cand.leg,
                    evidence,
                    t: None,
                    merge,
                }
            }
            AnyState::KobCondBid(_) => {
                let merge = match f.cand.merge {
                    Some(e) => {
                        let eo = get(&e)?;
                        Some(SellFirstEntry {
                            entry: typed(&eo.order, |s| if let AnyState::KobIfdAsk(x) = s { Some(x.clone()) } else { None })?,
                            custody: eo.custody.clone(),
                        })
                    }
                    None => None,
                };
                Leg::CondBid {
                    order: typed(&o.order, |s| if let AnyState::KobCondBid(x) = s { Some(x.clone()) } else { None })?,
                    amount,
                    leg: f.cand.leg,
                    evidence,
                    t: None,
                    merge,
                }
            }
            AnyState::KobIfdBid(_) => Leg::IfdBid {
                order: typed(&o.order, |s| if let AnyState::KobIfdBid(x) = s { Some(x.clone()) } else { None })?,
                amount,
                evidence,
                t: None,
            },
            AnyState::KobIfdAsk(_) => Leg::IfdAsk {
                order: typed(&o.order, |s| if let AnyState::KobIfdAsk(x) = s { Some(x.clone()) } else { None })?,
                custody: custody()?,
                amount,
                evidence,
                t: None,
            },
            AnyState::KobPair(_) => Leg::Pair {
                order: typed(&o.order, |s| if let AnyState::KobPair(x) = s { Some(x.clone()) } else { None })?,
                custody: custody()?,
                amount,
                t: None,
            },
            AnyState::KobCondPair(_) => {
                let merge = match f.cand.merge {
                    Some(e) => {
                        let eo = get(&e)?;
                        let tk = eo.order.state.pair_tokens().ok_or("the merged entry is not a pair entry")?;
                        Some(PairEntryMerge {
                            entry: typed(&eo.order, |s| if let AnyState::KobIfdPair(x) = s { Some(x.clone()) } else { None })?,
                            a_custody: eo.custody_of(&tk.a.cov_id).cloned(),
                            b_custody: eo.custody_of(&tk.b.cov_id).cloned(),
                        })
                    }
                    None => None,
                };
                Leg::CondPair {
                    order: typed(&o.order, |s| if let AnyState::KobCondPair(x) = s { Some(x.clone()) } else { None })?,
                    custody: custody()?,
                    amount,
                    leg: f.cand.leg,
                    evidence,
                    evidence_b: f.evidence_b,
                    t: None,
                    merge,
                }
            }
            AnyState::KobIfdPair(s) => {
                let tk = s.tokens();
                Leg::IfdPair {
                    order: typed(&o.order, |s| if let AnyState::KobIfdPair(x) = s { Some(x.clone()) } else { None })?,
                    a_custody: if s.is_buy_first() { None } else { o.custody_of(&tk.a.cov_id).cloned() },
                    b_custody: o.custody_of(&tk.b.cov_id).cloned(),
                    amount,
                    evidence,
                    evidence_b: f.evidence_b,
                    t: None,
                }
            }
            _ => return Err("not an order kind of either family".into()),
        };
        legs.push(leg);
    }
    let mut updates = vec![];
    for u in &plan.updates {
        let o = get(&u.id)?;
        if super::book::outpoint(&o.order.utxo) != u.outpoint {
            return Err(format!("order {} moved since it was planned", kob_protocol::json::to_hex(&u.id)));
        }
        updates.push(BatchUpdate { order: o.order.clone(), evidence: u.evidence, evidence_b: u.evidence_b, take: Some(u.take) });
    }
    Ok(Batch {
        lock_time: plan.lock_time,
        legs,
        updates,
        taker_tokens: vec![],
        taker: Some(cx.operator),
        taker_token_carrier: cx.token_carrier,
        receivers: vec![],
        payments: vec![],
        funding: cx.funding.clone(),
        change: Some(cx.operator),
        records: vec![],
        fee: FeeOptions::rate(cx.fee_rate),
    })
}

static BUDGET_SLACK_RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many builds had to be retried with more compute budget than the table gives (the matcher: the measured budgets of
/// [`measured_floors`]; a keeper job: one unit of slack). The table is generated exact over every builder shape in its
/// largest batch context (`kob-protocol/tests/budget_table.rs`), so this is a logged safety net only: it stays 0 in every
/// test, and a non-zero value in production is a shape the generator does not cover yet.
pub fn budget_slack_retries() -> u64 {
    BUDGET_SLACK_RETRIES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Records one safety-net retry (see [`budget_slack_retries`]).
pub(crate) fn note_budget_slack(what: &str, error: &str) {
    BUDGET_SLACK_RETRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::warn!(
        what,
        error,
        "compute budget table short for this shape (report it: kob-protocol's budget generator misses it); retrying with more budget"
    );
}

/// The budgets the inputs of a built transaction need when the table gives one of them too little: the transaction is
/// signed and run in the script engine without enforcing budgets, and every role with an input whose measured script
/// units exceed the budget it commits gets the budget that covers them (the largest over its inputs). An input's script
/// units do not depend on the budgets committed around it, so a rebuild with these budgets passes. Empty when every input
/// fits its budget.
pub fn measured_floors(
    built: &kob_protocol::tx::BuiltTx,
    signer: &dyn super::wallet::Signer,
) -> Result<BTreeMap<String, u16>, String> {
    use kob_protocol::tx::{finalize, FinalizeOptions};
    let sigs = signer.sign(built)?;
    let signed = finalize(built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    let (tx, entries) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    let units = kob_protocol::verify::measure_units(&tx, &entries).map_err(|e| e.to_string())?;
    let mut out: BTreeMap<String, u16> = BTreeMap::new();
    for ((role, u), inp) in built.roles.iter().zip(units).zip(&tx.inputs) {
        let need = kob_protocol::budget::budget_for_units(u);
        if need > inp.compute_commit.compute_budget().unwrap_or(0) {
            let e = out.entry(role.clone()).or_insert(0);
            *e = (*e).max(need);
        }
    }
    Ok(out)
}

/// Compute-budget lookup: the committed table, and for a role the table has not measured (an
/// unusual branch combination) the largest budget measured for the same template entry and token
/// program plus one unit. The engine validation before submission checks the result.
pub fn budgets(role: &str) -> kob_protocol::Result<u16> {
    if let Ok(b) = kob_protocol::budget::lookup(role) {
        return Ok(b);
    }
    let table = kob_protocol::budget::table();
    let (lhs, program) = match role.split_once('@') {
        Some((l, p)) => (l, Some(p)),
        None => (role, None),
    };
    let parts: Vec<&str> = lhs.split('.').collect();
    let prefix = if program.is_some() {
        parts.iter().take(2).copied().collect::<Vec<_>>().join(".")
    } else {
        parts.iter().take(3).copied().collect::<Vec<_>>().join(".")
    };
    table
        .iter()
        .filter(|(k, _)| {
            let (kl, kp) = match k.split_once('@') {
                Some((l, p)) => (l, Some(p)),
                None => (k.as_str(), None),
            };
            kp == program && (kl == prefix || kl.starts_with(&format!("{prefix}.")))
        })
        .map(|(_, v)| *v)
        .max()
        .map(|v| v.saturating_add(1))
        .ok_or_else(|| kob_protocol::Error::MissingBudget(role.to_string()))
}

/// `(need, have)` of a builder's "insufficient funds" refusal (`kob_protocol::Error::InsufficientFunds`, which lowering
/// reports as text): the batch's funding did not cover its outputs and fee.
pub fn insufficient_funds(err: &str) -> Option<(u64, u64)> {
    const NEED: &str = "insufficient funds: need ";
    let num = |s: &str| -> Option<u64> { s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok() };
    let rest = &err[err.find(NEED)? + NEED.len()..];
    let need = num(rest)?;
    let have = num(&rest[rest.find("have ")? + "have ".len()..])?;
    Some((need, have))
}

/// The exact economics of a built transaction for the operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accounting {
    /// Network fee paid.
    pub fee: u64,
    /// Operator change output value (0 when none).
    pub change: u64,
    /// Σ operator funding inputs.
    pub funding: u64,
    /// `change − funding`: the operator's KAS profit.
    pub profit: i64,
    pub bytes: u64,
}

/// KAS the operator puts on and takes back from its own token UTXOs in a batch: (Σ its token
/// inputs, the carriers of its net token outputs: one per token of which it receives some).
pub fn operator_token_kas(plan: &Plan, batch: &Batch) -> (u64, u64) {
    // per token: base units released into the batch (asks, pair sales) − base units delivered (bids, pair receipts)
    let mut net: BTreeMap<[u8; 32], i128> = BTreeMap::new();
    for f in &plan.fills {
        let q = f.amount as i128;
        match (&f.cand.pair, f.cand.side) {
            (Some(x), _) if x.role != PairRole::Surplus => {
                let i = &x.info;
                *net.entry(i.s_market().token).or_default() += i.sell_qty(f.amount).unwrap_or(0) as i128;
                *net.entry(i.t_market().token).or_default() -= i.buy_qty(f.amount).unwrap_or(0) as i128;
            }
            (Some(_), _) => {}
            (None, Side::Ask) => *net.entry(f.cand.book.token).or_default() += q,
            (None, Side::Bid) => *net.entry(f.cand.book.token).or_default() -= q,
        }
    }
    for t in &batch.taker_tokens {
        if let Some(c) = t.utxo.covenant_id {
            *net.entry(c).or_default() += t.state.amount() as i128;
        }
    }
    // a token a pair ask buys: its surplus goes to that ask's delivery (a minimum), no operator output
    for f in &plan.fills {
        if let Some(x) = &f.cand.pair {
            if x.role != PairRole::Surplus && !x.info.buy_exact() {
                net.remove(&x.info.t_market().token);
            }
        }
    }
    let kin = batch.taker_tokens.iter().map(|t| t.utxo.amount).sum();
    let kout = net.values().filter(|v| **v > 0).count() as u64 * batch.taker_token_carrier;
    (kin, kout)
}

/// Accounting of a built plan: `profit = change + own token carrier out − funding − own token carriers in`.
pub fn accounting(lowered: &super::family::Lowered, funding: &[KeyUtxo]) -> Accounting {
    let built = &lowered.built;
    let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
    let f: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    let profit = change as i64 + lowered.operator_token_kas_out as i64 - f as i64 - lowered.operator_token_kas_in as i64;
    Accounting { fee: built.fee.fee, change, funding: f, profit, bytes: built.fee.mass.size }
}
