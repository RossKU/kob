//! Keepers (`docs/spec/matcher.md` §5, §1.2): permissionless maintenance paid by the orders' own tips.
//!
//! | Job | When | Paid by |
//! |---|---|---|
//! | refund | soft expiry, 90 days idle, repeating entries (`KobIfdBid.refund`, `KobIfdAsk.settle(0)`) | `refundTip` |
//! | kill | IOC / FOK at `max(UTXO DAA, activeFrom) + 600` (first block that allows it), IOC / FOK pair orders included | `refundTip` |
//! | close | an empty repeating `KobIfdAsk` (`amountLeft = 0`) | `refundTip` |
//! | sweep | strays of the operator's own orders (cancel-replace; nobody else may move strays) | — |
//!
//! Arming and trailing are not keeper jobs (protocol v2.6): an `update` reads its trigger evidence from a plain resting fill
//! of the same transaction, so the matcher arms and ratchets stops inside the batches that fill their evidence
//! (`crate::matcher::batch`, §4) and takes their `keeperTip` there.
//!
//! Day orders need no special keeper: conforming matchers stop at the placement record's UTC
//! `deadline` (§2.3) and the refund opens at `expiryDaa`. Repeat-IFD merges are fills and belong
//! to the matcher, which sequences them (one merge per entry per transaction; the next exit merges
//! once the entry's continuation is accepted). Strays of other makers are only reported.
//!
//! Every job is built with `kob_protocol` builders, signed with the hot key and validated in the
//! v2.1.0 engine before submission, exactly like matcher batches.

use std::collections::{BTreeSet, VecDeque};

use kob_protocol::build::{build_with, Action, ForeignStrays, RefundOrder, SweepOrder};
use kob_protocol::state::*;
use kob_protocol::tx::{finalize, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo, OrderUtxo, SignedTx, TokenUtxo};
use kob_protocol::verify::{validate_signed, Validation};
use serde::{Deserialize, Serialize};

use crate::matcher::book::{outpoint, Clock, CovId, ListedOrder, Outpoint};
use crate::matcher::chain::UNACCEPTED_DAA;
use crate::matcher::lower::budgets;
use crate::matcher::wallet::Signer;

/// Keeper settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeeperConfig {
    /// Kept for configuration files (refunds use the exact refund time).
    pub safety_margin: u64,
    /// The lowest fee rate, sompi per gram (the floor every job may fall back to).
    pub fee_rate: u64,
    /// The tick's fee rates (`crate::fee`, set by the runner from the node's estimate): a job is built at its kind's urgency
    /// ([`JobKind::urgency`]), held to the total cap, and a paid job is lowered to the highest rate its tip still pays (not
    /// below `fee_rate`). The default (the floor everywhere, no cap) builds every job at `fee_rate`.
    pub fees: crate::fee::FeeRates,
    pub refund: bool,
    /// Sweep strays of the operator's own orders in place (`SweepOrder`: the order continues unchanged).
    pub sweep_own_strays: bool,
    /// Return an order's proven FOREIGN strays (tokens other than its own, `matcher.md` §1.2) to its maker inside the refund,
    /// kill or close that ends it: their programs authorise them with any spend of the order and no refund path reads
    /// another token, so this is permissionless, and after the order ends nothing could move them again. The maker's own
    /// token strays are never touched (every refund path refuses them; only the maker's cancel or sweep moves them). The
    /// extra inputs cost fee out of the refund tip: a job that does not pay with them is built without them.
    pub return_foreign_strays: bool,
    /// Smallest profit (tip − fee, sompi) a job must make.
    pub min_profit: i64,
    /// Most jobs per tick.
    pub max_jobs: usize,
    /// Most jobs BUILT (signed and engine-validated) per tick, successful or not: a dust order is a build attempt
    /// that yields no job, and building costs about 1.6 ms of CPU.
    pub max_attempts: usize,
    pub validate: bool,
}

impl Default for KeeperConfig {
    fn default() -> Self {
        KeeperConfig {
            safety_margin: 5,
            fee_rate: 100,
            fees: crate::fee::FeeRates::default(),
            refund: true,
            sweep_own_strays: false,
            return_foreign_strays: false,
            min_profit: 0,
            max_jobs: 32,
            max_attempts: 128,
            validate: true,
        }
    }
}

/// Kind of keeper job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    Kill,
    Refund,
    Close,
    Sweep,
}

impl JobKind {
    /// How soon the job should be accepted (`crate::fee`): a kill ends an IOC / FOK order at its deadline (high), a refund
    /// returns a maker's funds (normal), a close or a sweep is housekeeping (low).
    pub fn urgency(self) -> crate::fee::Urgency {
        use crate::fee::Urgency;
        match self {
            JobKind::Kill => Urgency::High,
            JobKind::Refund => Urgency::Normal,
            JobKind::Close | JobKind::Sweep => Urgency::Low,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            JobKind::Kill => "kill",
            JobKind::Refund => "refund",
            JobKind::Close => "close",
            JobKind::Sweep => "sweep",
        }
    }
}

/// A signed, validated keeper transaction.
#[derive(Clone, Debug)]
pub struct Job {
    pub kind: JobKind,
    pub order: CovId,
    pub action: Action,
    pub built: BuiltTx,
    pub signed: SignedTx,
    pub validation: Option<Validation>,
    /// Operator profit: change − funding (sompi).
    pub profit: i64,
    pub spends: BTreeSet<Outpoint>,
}

/// Snapshot the keepers work on.
pub struct KeeperInput {
    pub orders: Vec<ListedOrder>,
    pub clock: Clock,
    pub funding: Vec<KeyUtxo>,
    pub excluded: BTreeSet<CovId>,
    pub excluded_outpoints: BTreeSet<Outpoint>,
    /// (order, order outpoint) pairs an earlier tick found unprofitable at their fixed tip: not built again while the
    /// outpoint stands. The runner keeps the set from [`KeeperReport::unprofitable`].
    pub known_unprofitable: BTreeSet<(CovId, Outpoint)>,
}

/// Result of a keeper tick.
#[derive(Debug, Default)]
pub struct KeeperReport {
    pub jobs: Vec<Job>,
    /// Orders with strays the operator may not sweep (reported for monitoring).
    pub foreign_strays: Vec<CovId>,
    pub skipped: Vec<(CovId, String)>,
    /// Jobs built and found unprofitable at the order's fixed tip, for [`KeeperInput::known_unprofitable`].
    pub unprofitable: Vec<(CovId, Outpoint)>,
    /// Jobs that could not be built, signed or validated (a refund tip above the order's value, a token program that refuses
    /// to move the custody, ...): (order, order outpoint). The runner leaves the outpoint out of the next ticks with a growing
    /// backoff (`excluded_outpoints`), so unbuildable orders cannot eat the attempt budget of every tick and starve the
    /// buildable kills and refunds queued behind them.
    pub failed: Vec<(CovId, Outpoint)>,
}

/// Cheap lower bound of what a refund of `o` costs in fees: the order input and its payout output at the standard mass floor
/// (twice the size), without a funding input (a keeper builds a refund without one when the funded one does not pay). A
/// refund tip below it can never be profitable, so the transaction is not even built.
fn refund_fee_floor(o: &ListedOrder, cfg: &KeeperConfig) -> i64 {
    use crate::matcher::candidate::{redeem_len, INPUT_BYTES, OUTPUT_BYTES};
    let bytes = INPUT_BYTES + redeem_len(o.order.state.template_id()) + OUTPUT_BYTES;
    (cfg.fee_rate.saturating_mul(2 * bytes)).min(i64::MAX as u64) as i64
}

fn typed_order(o: &ListedOrder) -> OrderUtxo<AnyState> {
    o.order.clone()
}

/// The refund job of an order due at `daa` (None when not due).
pub fn refund_job(o: &ListedOrder, daa: u64) -> Option<(JobKind, u64)> {
    let udaa = o.utxo_daa();
    // the refund time adds to chain-supplied fields; a state the numeric gate refuses has none (the tick sorts
    // every order by it before the gate)
    if udaa == UNACCEPTED_DAA || udaa > i64::MAX as u64 || crate::sanity::check(&o.order.state).is_err() {
        return None;
    }
    let due = o.order.state.refund_due(udaa as i64)?;
    if due < 0 || (daa as i64) < due {
        return None;
    }
    let kind = match &o.base_state() {
        AnyState::KobIfdAsk(s) if s.amount_left == 0 => JobKind::Close,
        AnyState::KobAsk(s) if s.tif != TIF_GTC && due < s.expiry_daa => JobKind::Kill,
        AnyState::KobBid(s) if s.tif != TIF_GTC && due < s.expiry_daa => JobKind::Kill,
        // a pair order is refunded (an IOC / FOK one killed) like an ask: its custodies back to the maker (a sell-first
        // entry's A custody and B prefund at their own indices), never its strays (strays of either token move only with
        // the maker's cancel or a sweep)
        AnyState::KobPair(s) if s.tif != TIF_GTC && due < s.expiry_daa => JobKind::Kill,
        _ => JobKind::Refund,
    };
    Some((kind, due as u64))
}

/// A sweep IN PLACE of one of the operator's own orders: the order continues unchanged (same state, covenant id and custody;
/// a `SWEEP` record keeps it listed) and its strays go back to the operator, the maker. Strays of the order's token (a pair
/// order's: either token) that share one extension commitment, and its proven foreign strays, each within the program's
/// token inputs; whatever does not fit is swept by a later tick. Only a plain ask may pay from its carrier: every other kind
/// needs the funding.
fn sweep_request(o: &ListedOrder, funding: Option<&KeyUtxo>, fee: &FeeOptions) -> Option<SweepOrder> {
    let carrier_pays = matches!(o.order.state, AnyState::KobAsk(_) | AnyState::KobAskKron(_));
    if funding.is_none() && !carrier_pays {
        return None;
    }
    // token inputs one transfer of that token may carry (its program's; 4, the smallest, when unknown)
    let slots = |tok: [u8; 32]| -> usize {
        let hash = if tok == o.order.state.token_cov_id() {
            o.order.state.token_tpl_hash()
        } else {
            o.order.state.pair_tokens().filter(|x| x.b.cov_id == tok).map(|x| x.b.tpl_hash)
        };
        hash.and_then(|h| kob_protocol::artifacts::token_template_by_hash(&h)).map(|t| t.slots.0).unwrap_or(4)
    };
    let mut strays: Vec<TokenUtxo> = vec![];
    for tok in o.strays.iter().filter_map(|s| s.utxo.covenant_id).collect::<BTreeSet<_>>() {
        let group: Vec<&TokenUtxo> = o.strays.iter().filter(|s| s.utxo.covenant_id == Some(tok) && s.state.is_plain()).collect();
        let Some(first) = group.first() else { continue };
        let ext = first.state.extension();
        let cap = slots(tok);
        strays.extend(group.into_iter().filter(|s| s.state.extension() == ext).take(cap).cloned());
    }
    let foreign = returnable_foreign(o);
    if strays.is_empty() && foreign.is_empty() {
        return None;
    }
    Some(SweepOrder {
        order: typed_order(o),
        strays,
        foreign,
        funding: funding.cloned().into_iter().collect(),
        change: None,
        token_carrier: None,
        lock_time: 0,
        records: vec![],
        fee: fee.clone(),
    })
}

/// Builds, signs and validates a job. The budget table is exact over every generated shape; a budget
/// one unit short is retried once with one unit of slack as a logged, counted safety net
/// ([`crate::matcher::lower::budget_slack_retries`], 0 in every test).
fn finish(
    action: Action,
    signer: &dyn Signer,
    validate: bool,
    funding: &[KeyUtxo],
) -> Result<(BuiltTx, SignedTx, Option<Validation>, i64), String> {
    match finish_with(&action, signer, validate, funding, 0) {
        Err(e) if e.contains("ExceededCommittedScriptUnits") => {
            crate::matcher::lower::note_budget_slack("keeper job", &e);
            finish_with(&action, signer, validate, funding, 1)
        }
        r => r,
    }
}

fn finish_with(
    action: &Action,
    signer: &dyn Signer,
    validate: bool,
    funding: &[KeyUtxo],
    slack: u16,
) -> Result<(BuiltTx, SignedTx, Option<Validation>, i64), String> {
    let with_slack = move |role: &str| budgets(role).map(|b| b.saturating_add(slack));
    let built = build_with(action, &with_slack).map_err(|e| e.to_string())?;
    let sigs = signer.sign(&built)?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    let v = if validate { Some(validate_signed(&signed).map_err(|e| format!("engine rejected: {e}"))?) } else { None };
    let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
    let f: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    Ok((built, signed, v, change as i64 - f as i64))
}

/// The fee rate a job is built at (`crate::fee`): its urgency's rate, held to the total cap and, for a paid job, to the
/// highest rate its tip still pays (at least the floor). Priced from an unsigned build at the floor and checked with one at
/// the rate found (none while the urgency's rate is the floor); a job that does not build or pay there goes at the floor.
fn job_rate(action: &Action, kind: JobKind, cfg: &KeeperConfig, funding: &[KeyUtxo]) -> u64 {
    let floor = cfg.fees.floor_of(cfg.fee_rate);
    let want = cfg.fees.rate(kind.urgency(), cfg.fee_rate);
    if want <= floor {
        return floor;
    }
    let funded: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    let paid = kind != JobKind::Sweep;
    let profit_at = |a: &Action| -> Option<(u64, i64)> {
        let built = build_with(a, &budgets).ok()?;
        let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
        Some((built.fee.fee, change as i64 - funded as i64))
    };
    let mut a = action.clone();
    crate::fee::set_rate(&mut a, floor);
    let Some((fee0, profit0)) = profit_at(&a) else { return floor };
    let rate = cfg.fees.priced(want, floor, fee0, paid.then_some(profit0), cfg.min_profit);
    if rate <= floor {
        return floor;
    }
    crate::fee::set_rate(&mut a, rate);
    match profit_at(&a) {
        Some((fee, profit)) if (!paid || profit >= cfg.min_profit) && (cfg.fees.max_tx_fee == 0 || fee <= cfg.fees.max_tx_fee) => rate,
        _ => floor,
    }
}

/// The foreign strays of an order one transaction can move: per token, the plain UTXOs that share the first one's extension
/// commitment, within the program's token inputs (the rest wait for a later transaction).
fn returnable_foreign(o: &ListedOrder) -> Vec<ForeignStrays> {
    o.foreign
        .iter()
        .filter_map(|g| {
            let cap = g.token.program.token_slots()?.0;
            let first = g.utxos.iter().find(|s| s.state.is_plain())?;
            let ext = first.state.extension();
            let utxos: Vec<TokenUtxo> =
                g.utxos.iter().filter(|s| s.state.is_plain() && s.state.extension() == ext).take(cap).cloned().collect();
            Some(ForeignStrays { token: g.token.clone(), utxos })
        })
        .collect()
}

/// Drops the foreign strays a keeper refund would return. `false` when it returns none.
fn strip_foreign(a: &mut Action) -> bool {
    match a {
        Action::RefundOrder(r) if !r.foreign.is_empty() => {
            r.foreign.clear();
            true
        }
        _ => false,
    }
}

/// Drops the funding inputs of a keeper job (a refund then pays its fee from the order's tip alone). `false` when the job has
/// no funding to drop.
fn strip_funding(a: &mut Action) -> bool {
    match a {
        Action::RefundOrder(r) if !r.funding.is_empty() => {
            r.funding.clear();
            true
        }
        _ => false,
    }
}

fn spends_of(built: &BuiltTx) -> BTreeSet<Outpoint> {
    built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect()
}

/// Plans, builds, signs and validates every keeper job of a snapshot. Funding is chained through
/// each job's change output (a P2PK spend never depends on its parent's DAA score).
pub fn tick(inp: &KeeperInput, cfg: &KeeperConfig, signer: &dyn Signer) -> KeeperReport {
    let mut report = KeeperReport::default();
    let operator = signer.pubkey();
    let daa = inp.clock.daa;
    let fee = FeeOptions::rate(cfg.fee_rate);
    let mut pool: VecDeque<KeyUtxo> = inp.funding.iter().cloned().collect();
    let mut attempts = 0usize;
    let mut orders: Vec<&ListedOrder> = inp.orders.iter().collect();
    // Deterministic: kills first (the first block after the kill time), then by refund time / id.
    orders.sort_by_key(|o| match refund_job(o, daa) {
        Some((k, due)) => (0u8, k, due, o.id()),
        None => (1u8, JobKind::Sweep, 0, o.id()),
    });
    for o in orders {
        if report.jobs.len() >= cfg.max_jobs {
            break;
        }
        let id = o.id();
        if inp.excluded.contains(&id) || inp.excluded_outpoints.contains(&outpoint(&o.order.utxo)) {
            continue;
        }
        // numbers the builders cannot survive are never built from
        if let Err(why) = crate::sanity::check(&o.order.state) {
            report.skipped.push((id, format!("not serviced: {why}")));
            continue;
        }
        if attempts >= cfg.max_attempts {
            report.skipped.push((id, "keeper attempt budget of this tick used up".into()));
            break;
        }
        if (!o.strays.is_empty() || !o.foreign.is_empty()) && o.order.state.maker() != operator {
            report.foreign_strays.push(id);
        }
        let funding = pool.front().cloned();
        let funding_vec: Vec<KeyUtxo> = funding.iter().cloned().collect();
        let mut attempt: Option<(JobKind, Action)> = None;
        let known = |o: &ListedOrder| inp.known_unprofitable.contains(&(o.id(), outpoint(&o.order.utxo)));
        if cfg.refund {
            if let Some((kind, due)) = refund_job(o, daa) {
                // a refund that pays the keeper less than the fee floor is never worth building
                let tip = crate::model::terms_of(&o.order.state).refund_tip;
                if known(o) || tip.saturating_sub(refund_fee_floor(o, cfg)) < cfg.min_profit {
                    report.skipped.push((id, format!("{}: tip {tip} does not cover the fee", kind.name())));
                    continue;
                }
                // the tip is paid out of the order UTXO: a tip above its value can never be built (the maker's cancel still
                // works). Not worth an attempt; the runner remembers it.
                if tip > i64::try_from(o.order.utxo.amount).unwrap_or(i64::MAX) {
                    report.skipped.push((id, format!("{}: refund tip {tip} exceeds the order's value", kind.name())));
                    report.failed.push((id, outpoint(&o.order.utxo)));
                    continue;
                }
                if !o.custody_ok() && o.order.state.custody_amount().unwrap_or(0) != 0 {
                    // reported, not silently skipped: only the maker's cancel (or a later custody) ends it
                    report
                        .skipped
                        .push((id, format!("{}: the custody is missing or does not hold the order's amountLeft", kind.name())));
                } else {
                    attempt = Some((
                        kind,
                        Action::RefundOrder(RefundOrder {
                            order: typed_order(o),
                            foreign: if cfg.return_foreign_strays { returnable_foreign(o) } else { vec![] },
                            custody: o.custody.clone().filter(|_| o.order.state.custody_amount().unwrap_or(0) > 0),
                            prefund: o.custody_b.clone(),
                            lock_time: due,
                            funding: funding_vec.clone(),
                            change: Some(operator),
                            fee: fee.clone(),
                        }),
                    ));
                }
            }
        }
        if attempt.is_none()
            && cfg.sweep_own_strays
            && o.order.state.maker() == operator
            && (!o.strays.is_empty() || !o.foreign.is_empty())
        {
            if let Some(req) = sweep_request(o, funding.as_ref(), &fee) {
                attempt = Some((JobKind::Sweep, Action::SweepOrder(req)));
            }
        }
        let Some((kind, mut action)) = attempt else { continue };
        attempts += 1;
        let rate = job_rate(&action, kind, cfg, &funding_vec);
        crate::fee::set_rate(&mut action, rate);
        let mut funding = funding;
        let mut result = finish(action.clone(), signer, cfg.validate, &funding_vec);
        // A paid job that loses money only because of its funding input (its mass, a change output) or fails or loses money
        // only because of the foreign strays it returns is rebuilt without them: the order's own tip then pays the fee and
        // the keeper keeps what is left (C5 K-7). Only a job that loses money every way is remembered as unprofitable.
        let poor = |r: &Result<(BuiltTx, SignedTx, Option<Validation>, i64), String>| match r {
            Ok((.., p)) => *p < cfg.min_profit,
            Err(_) => true,
        };
        if kind != JobKind::Sweep && poor(&result) {
            for (no_funding, no_foreign) in [(true, false), (false, true), (true, true)] {
                let mut alt = action.clone();
                if (no_funding && !strip_funding(&mut alt)) || (no_foreign && !strip_foreign(&mut alt)) {
                    continue;
                }
                let fv: &[KeyUtxo] = if no_funding { &[] } else { &funding_vec };
                if let Ok(r) = finish(alt.clone(), signer, cfg.validate, fv) {
                    if r.3 >= cfg.min_profit {
                        action = alt;
                        if no_funding {
                            funding = None;
                        }
                        result = Ok(r);
                        break;
                    }
                }
            }
        }
        match result {
            Ok((built, signed, validation, profit)) => {
                if kind != JobKind::Sweep && profit < cfg.min_profit {
                    report.skipped.push((id, format!("{}: tip does not cover the fee ({profit})", kind.name())));
                    report.unprofitable.push((id, outpoint(&o.order.utxo)));
                    continue;
                }
                if funding.is_some() {
                    pool.pop_front();
                    if let Some(i) = built.fee.change_output {
                        let v = built.tx.outputs[i as usize].value;
                        let u = kob_protocol::tx::Utxo {
                            transaction_id: built.tx.id,
                            index: i,
                            amount: v,
                            block_daa_score: UNACCEPTED_DAA,
                            covenant_id: None,
                        };
                        pool.push_back(KeyUtxo { utxo: u, pubkey: operator });
                    }
                }
                let spends = spends_of(&built);
                report.jobs.push(Job { kind, order: id, action, built, signed, validation, profit, spends });
            }
            Err(e) => {
                report.skipped.push((id, format!("{}: {e}", kind.name())));
                report.failed.push((id, outpoint(&o.order.utxo)));
            }
        }
    }
    report
}
