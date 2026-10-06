//! Operator maintenance (`docs/ops/executor.md`, maintenance): the operator's own token UTXOs.
//!
//! The operator can come to hold token UTXOs of its own key (protocol v2.6 routes kept the remainder of their purchases of B
//! there; today a route delivers every base unit of B it buys, but tokens may still reach the operator's key, and the
//! matcher's opt-in surplus-inventory policy, `PlannerConfig::inventory`, accumulates pair surpluses there on purpose),
//! each with a KAS carrier on it (`EngineConfig::token_carrier`, 10 KAS by default). Left alone they pile up: in the TN10 soak
//! (2026-10-01) each executor held 22 to 28 of them after 51 minutes, 220 to 280 KAS of carriers locked for a few cents of
//! tokens, while the bank's top-ups ran into them.
//!
//! | Job | When | What |
//! |---|---|---|
//! | sell | the token is not held as surplus inventory (`inventory` lists it: the owner sells those off-matcher), the operator holds at least one minimum fill of a bid that is left after the matcher's tick, and selling into it pays more than the fee | its token UTXOs (as many as the program takes) are sold into the best such bid as a taker; the rest comes back as one token UTXO |
//! | merge | otherwise, the operator holds at least `min_utxos` token UTXOs of one token | as many as the program takes become one: the other carriers return to the funding pool |
//!
//! Both are bounded per tick (`max_jobs`), built with the `kob_protocol` builders (a taker `Batch`, `SendTokens`), signed with
//! the hot key and validated in the v2.1.0 engine before submission, like every matcher and keeper transaction. KRON tokens
//! held by address presence need a P2PK input of the operator: those jobs take the smallest funding UTXO (its change returns
//! it); KCC-20 jobs pay their fee from the carriers they free.

use std::collections::{BTreeMap, BTreeSet};

use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build_with, Action, Batch, Leg, SendTokens, TokenRecipient, TokenRef};
use kob_protocol::state::AnyState;
use kob_protocol::tx::{finalize, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo, OrderUtxo, SignedTx, TokenUtxo};
use kob_protocol::verify::{validate_signed, Validation};
use serde::{Deserialize, Serialize};

use crate::matcher::book::{book_key, outpoint, Clock, CovId, ListedOrder, Outpoint};
use crate::matcher::candidate::{candidates_of, CandCtx, Side};
use crate::matcher::family::token_limits;
use crate::matcher::lower::budgets;
use crate::matcher::wallet::Signer;

/// Maintenance settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MaintenanceConfig {
    /// Run the maintenance jobs at all (on by default).
    pub enabled: bool,
    /// Merge the operator's token UTXOs of one token.
    pub merge: bool,
    /// Sell the operator's tokens into the book (at least a bid's minimum fill) when that pays more than the fee.
    pub sell: bool,
    /// Smallest number of token UTXOs of one token worth a merge.
    pub min_utxos: usize,
    /// Most maintenance transactions per tick.
    pub max_jobs: usize,
    /// Smallest profit (proceeds − fee, sompi) of a sale.
    pub min_sell_profit: i64,
    /// The lowest fee rate, sompi per gram.
    pub fee_rate: u64,
    /// The tick's fee rates (`crate::fee`, set by the runner): every job is housekeeping and goes at the low rate, held to the
    /// total cap; a sale is lowered to the highest rate it still pays (not below `fee_rate`). The default (the floor
    /// everywhere, no cap) builds every job at `fee_rate`.
    pub fees: crate::fee::FeeRates,
    /// KAS carrier on the token UTXO a job leaves (the merged one, a sale's remainder).
    pub token_carrier: u64,
    /// `t = DAA − margin` of a sale's lock time (the matcher's margin).
    pub safety_margin: u64,
    pub validate: bool,
    /// The matcher's surplus-inventory policy (`PlannerConfig::inventory`): a token it lists is accumulated inventory that
    /// the owner sells off-matcher, so it is never sold here (merged only), whether or not the switch is on.
    pub inventory: crate::matcher::planner::InventoryPolicy,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        MaintenanceConfig {
            enabled: true,
            merge: true,
            sell: true,
            min_utxos: 2,
            max_jobs: 2,
            min_sell_profit: 0,
            fee_rate: 100,
            fees: crate::fee::FeeRates::default(),
            token_carrier: 1_000_000_000,
            safety_margin: 5,
            validate: true,
            inventory: crate::matcher::planner::InventoryPolicy::default(),
        }
    }
}

/// Kind of maintenance job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaintKind {
    Sell,
    Merge,
}

impl MaintKind {
    pub fn name(self) -> &'static str {
        match self {
            MaintKind::Sell => "sell",
            MaintKind::Merge => "merge",
        }
    }
}

/// An operator token UTXO and the program of its token (the indexer's proven holding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnToken {
    pub program: TemplateId,
    pub token: TokenUtxo,
}

/// Snapshot the maintenance jobs work on.
pub struct MaintenanceInput {
    /// The operator's key-owned token UTXOs (accepted).
    pub tokens: Vec<OwnToken>,
    /// The book (the bids a sale may fill).
    pub orders: Vec<ListedOrder>,
    pub clock: Clock,
    /// The operator's P2PK funding left after the tick's other transactions.
    pub funding: Vec<KeyUtxo>,
    /// Orders spent by pending transactions or backed off.
    pub excluded: BTreeSet<CovId>,
    /// Outpoints spent by pending transactions.
    pub excluded_outpoints: BTreeSet<Outpoint>,
}

/// A signed, validated maintenance transaction.
#[derive(Clone, Debug)]
pub struct MaintJob {
    pub kind: MaintKind,
    pub token: [u8; 32],
    /// The bid a sale fills.
    pub order: Option<CovId>,
    pub action: Action,
    pub built: BuiltTx,
    pub signed: SignedTx,
    pub validation: Option<Validation>,
    /// The operator's KAS after the job less before, its token carriers counted on both sides: a sale's proceeds less the
    /// fee, a merge's −fee.
    pub profit: i64,
    /// Carrier KAS the job moves from the operator's token UTXOs to its funding (the change).
    pub released: i64,
    pub spends: BTreeSet<Outpoint>,
}

/// Result of a maintenance tick.
#[derive(Debug, Default)]
pub struct MaintReport {
    pub jobs: Vec<MaintJob>,
    /// (token, why) of the jobs not built.
    pub skipped: Vec<([u8; 32], String)>,
}

/// One token's holdings, grouped for a job: same covenant id, program and extension (KCC-20 transfers keep it).
type Group = ([u8; 32], TemplateId, [u8; 32]);

fn build_sign(action: &Action, signer: &dyn Signer, validate: bool) -> Result<(BuiltTx, SignedTx, Option<Validation>), String> {
    let built = build_with(action, &budgets).map_err(|e| e.to_string())?;
    let sigs = signer.sign(&built)?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    let v = if validate { Some(validate_signed(&signed).map_err(|e| format!("engine rejected: {e}"))?) } else { None };
    Ok((built, signed, v))
}

/// `a` at its fee rate (`crate::fee`): the low rate, held to the total cap; with `profit` (a sale: the operator's KAS of the
/// built job) to the highest rate that keeps `min_sell_profit`, at least the floor. Priced from an unsigned build at the floor
/// and checked with one at the rate found (none while the low rate is the floor); a job that does not build or pay there
/// goes at the floor.
fn priced(mut a: Action, cfg: &MaintenanceConfig, profit: impl Fn(&BuiltTx) -> Option<i64>) -> Action {
    let floor = cfg.fees.floor_of(cfg.fee_rate);
    let want = cfg.fees.rate(crate::fee::Urgency::Low, cfg.fee_rate);
    crate::fee::set_rate(&mut a, floor);
    if want <= floor {
        return a;
    }
    let Ok(built) = build_with(&a, &budgets) else { return a };
    let rate = cfg.fees.priced(want, floor, built.fee.fee, profit(&built), cfg.min_sell_profit);
    if rate <= floor {
        return a;
    }
    let mut b = a.clone();
    crate::fee::set_rate(&mut b, rate);
    match build_with(&b, &budgets) {
        Ok(built)
            if profit(&built).is_none_or(|p| p >= cfg.min_sell_profit)
                && (cfg.fees.max_tx_fee == 0 || built.fee.fee <= cfg.fees.max_tx_fee) =>
        {
            b
        }
        _ => a,
    }
}

/// The operator's KAS balance change of a built job: change and token carrier outputs to the operator, less its funding
/// and token inputs.
fn operator_kas(built: &BuiltTx, funding: &[KeyUtxo], tokens: &[TokenUtxo], carrier_out: u64) -> i64 {
    let change = built.fee.change_output.map(|i| built.tx.outputs[i as usize].value).unwrap_or(0);
    let fin: u64 = funding.iter().map(|k| k.utxo.amount).sum();
    let tin: u64 = tokens.iter().map(|t| t.utxo.amount).sum();
    change as i64 + carrier_out as i64 - fin as i64 - tin as i64
}

/// The best bid of the book a sale of the operator's tokens of `g` may fill, how many base units and what it pays: a plain,
/// accepted, live `KobBid` of the token's market (program and extension), not excluded, whose quantity rules (its minimum
/// fill unless the fill ends it, FOK) accept a fill of at most `held` base units. Highest all-in price per base unit first,
/// then age and id.
fn best_bid(inp: &MaintenanceInput, g: &Group, held: i64, t: i64) -> Option<(ListedOrder, i64, i64)> {
    let (token, program, ext) = *g;
    let tpl = kob_protocol::artifacts::token_template(program).hash;
    let mine: Vec<&ListedOrder> = inp
        .orders
        .iter()
        .filter(|o| book_key(o).is_some_and(|k| k.token == token && k.template == tpl && k.extension == ext))
        .collect();
    let by_id: BTreeMap<CovId, ListedOrder> = mine.iter().map(|o| (o.id(), (*o).clone())).collect();
    let none = BTreeSet::new();
    let cx = CandCtx { t, utc: inp.clock.utc, excluded: &inp.excluded, by_id: &by_id, unaccepted: &none };
    let mut best: Option<(i128, i128, u64, CovId, usize, i64, i64)> = None;
    for c in candidates_of(&mine, &cx) {
        if c.side != Side::Bid || !c.is_plain() || c.trigger.is_some() {
            continue;
        }
        let o = mine[c.order];
        if inp.excluded_outpoints.contains(&outpoint(&o.order.utxo)) {
            continue;
        }
        let mut n = held.min(c.cap);
        if let Some((_, hi)) = c.fok {
            n = n.min(hi);
        }
        if n <= 0 || !c.quantity_ok(n) {
            continue;
        }
        let (pn, pd) = c.per_base();
        let better = match best {
            None => true,
            Some((bn, bd, age, id, ..)) => (pn * bd, std::cmp::Reverse((c.age, c.id))) > (bn * pd, std::cmp::Reverse((age, id))),
        };
        if better {
            best = Some((pn, pd, c.age, c.id, c.order, n, c.value(n).unwrap_or(0)));
        }
    }
    best.map(|(_, _, _, _, k, n, pays)| (mine[k].clone(), n, pays))
}

/// The sale of `tokens` (one group) into `bid` for `amount` base units, the operator as taker: the bid pays the operator's
/// change, the unsold rest comes back as one token UTXO.
fn sell_action(
    bid: &ListedOrder,
    amount: i64,
    tokens: &[TokenUtxo],
    funding: &[KeyUtxo],
    operator: [u8; 32],
    lock: u64,
    cfg: &MaintenanceConfig,
) -> Result<Action, String> {
    let base = bid.order.state.clone().into_family(kob_protocol::family::Family::Kcc20);
    let AnyState::KobBid(s) = base else { return Err("not a plain bid".into()) };
    Ok(Action::Batch(Batch {
        lock_time: lock,
        legs: vec![Leg::Bid { order: OrderUtxo { utxo: bid.order.utxo.clone(), state: s }, amount, t: None }],
        updates: vec![],
        taker_tokens: tokens.to_vec(),
        taker: Some(operator),
        taker_token_carrier: cfg.token_carrier,
        keep_surplus: vec![],
        receivers: vec![],
        payments: vec![],
        funding: funding.to_vec(),
        change: Some(operator),
        records: vec![],
        fee: FeeOptions::rate(cfg.fee_rate),
    }))
}

fn merge_action(g: &Group, tokens: &[TokenUtxo], funding: &[KeyUtxo], operator: [u8; 32], cfg: &MaintenanceConfig) -> Action {
    let amount: i64 = tokens.iter().map(|t| t.state.amount()).sum();
    Action::SendTokens(SendTokens {
        token: TokenRef { covenant_id: g.0, program: g.1 },
        tokens: tokens.to_vec(),
        recipients: vec![TokenRecipient { pubkey: operator, amount, carrier: cfg.token_carrier }],
        token_change: None,
        token_change_carrier: 0,
        funding: funding.to_vec(),
        change: Some(operator),
        records: vec![],
        fee: FeeOptions::rate(cfg.fee_rate),
    })
}

/// Plans, builds, signs and validates the maintenance jobs of a snapshot: per token (by covenant id), a sale when one pays,
/// else a merge, at most `max_jobs`. Deterministic.
pub fn tick(inp: &MaintenanceInput, cfg: &MaintenanceConfig, signer: &dyn Signer) -> MaintReport {
    let mut report = MaintReport::default();
    if !cfg.enabled || (!cfg.merge && !cfg.sell) {
        return report;
    }
    let operator = signer.pubkey();
    let t = inp.clock.lock_time(cfg.safety_margin);
    let mut groups: BTreeMap<Group, Vec<TokenUtxo>> = BTreeMap::new();
    for o in &inp.tokens {
        let s = &o.token.state;
        if !s.is_user() || !s.is_plain() || s.owner() != operator || s.amount() <= 0 || o.token.utxo.covenant_id.is_none() {
            continue;
        }
        if inp.excluded_outpoints.contains(&outpoint(&o.token.utxo)) || o.program.family() != s.family() {
            continue;
        }
        let g = (o.token.utxo.covenant_id.unwrap_or_default(), o.program, s.extension());
        groups.entry(g).or_default().push(o.token.clone());
    }
    // the smallest funding UTXO first (a KRON job only needs its presence; its change returns it)
    let mut pool: Vec<KeyUtxo> =
        inp.funding.iter().filter(|k| !inp.excluded_outpoints.contains(&outpoint(&k.utxo))).cloned().collect();
    pool.sort_by(|a, b| a.utxo.amount.cmp(&b.utxo.amount).then(outpoint(&a.utxo).cmp(&outpoint(&b.utxo))));
    for (g, mut utxos) in groups {
        if report.jobs.len() >= cfg.max_jobs {
            break;
        }
        let Some(lim) = token_limits(g.1.family(), &kob_protocol::artifacts::token_template(g.1).hash) else {
            report.skipped.push((g.0, format!("unsupported token program {}", g.1.name())));
            continue;
        };
        // as many as one transaction of the program takes, the oldest first
        utxos.sort_by(|a, b| (a.utxo.block_daa_score, outpoint(&a.utxo)).cmp(&(b.utxo.block_daa_score, outpoint(&b.utxo))));
        utxos.truncate(lim.max_in.max(1));
        let held: i64 = utxos.iter().map(|u| u.state.amount()).sum();
        let kron = g.1.family() == kob_protocol::family::Family::Kron;
        let funding: Vec<KeyUtxo> = if kron { pool.first().cloned().into_iter().collect() } else { vec![] };
        if kron && funding.is_empty() {
            report.skipped.push((g.0, "a KRON token job needs a P2PK input of the operator: no funding".into()));
            continue;
        }
        let carriers_in: i64 = utxos.iter().map(|u| u.utxo.amount as i64).sum();
        let mut job: Option<MaintJob> = None;
        // surplus inventory the matcher accumulates is the owner's to sell, off-matcher: never sold here (a merge only)
        let held_inventory = cfg.inventory.holds(&g.0);
        if held_inventory && cfg.sell {
            report.skipped.push((g.0, "sell: surplus inventory (the owner sells it off-matcher)".into()));
        }
        if cfg.sell && !held_inventory {
            if let Some((bid, amount, _)) = best_bid(inp, &g, held, t as i64) {
                let rest = held > 0 && held - amount > 0;
                let carrier_out = if rest { cfg.token_carrier } else { 0 };
                match sell_action(&bid, amount, &utxos, &funding, operator, t, cfg).and_then(|a| {
                    let a = priced(a, cfg, |b| Some(operator_kas(b, &funding, &utxos, carrier_out)));
                    let (built, signed, v) = build_sign(&a, signer, cfg.validate)?;
                    Ok((a, built, signed, v))
                }) {
                    Ok((action, built, signed, validation)) => {
                        // proceeds − fee (the carriers the sale frees are the operator's own KAS on both sides)
                        let profit = operator_kas(&built, &funding, &utxos, carrier_out);
                        if profit >= cfg.min_sell_profit {
                            let spends = built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
                            job = Some(MaintJob {
                                kind: MaintKind::Sell,
                                token: g.0,
                                order: Some(bid.id()),
                                action,
                                built,
                                signed,
                                validation,
                                profit,
                                released: carriers_in - carrier_out as i64,
                                spends,
                            });
                        } else {
                            report.skipped.push((g.0, format!("sell: the bid pays less than the fee ({profit})")));
                        }
                    }
                    Err(e) => report.skipped.push((g.0, format!("sell: {e}"))),
                }
            }
        }
        if job.is_none() && cfg.merge && utxos.len() >= cfg.min_utxos.max(2) {
            let action = priced(merge_action(&g, &utxos, &funding, operator, cfg), cfg, |_| None);
            match build_sign(&action, signer, cfg.validate) {
                Ok((built, signed, validation)) => {
                    let profit = operator_kas(&built, &funding, &utxos, cfg.token_carrier);
                    let spends = built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
                    job = Some(MaintJob {
                        kind: MaintKind::Merge,
                        token: g.0,
                        order: None,
                        action,
                        built,
                        signed,
                        validation,
                        profit,
                        released: carriers_in - cfg.token_carrier as i64,
                        spends,
                    });
                }
                Err(e) => report.skipped.push((g.0, format!("merge: {e}"))),
            }
        }
        if let Some(j) = job {
            // a funding UTXO a job spends is gone for the next one (its change is not accepted yet)
            pool.retain(|k| !j.spends.contains(&outpoint(&k.utxo)));
            report.jobs.push(j);
        }
    }
    report
}
