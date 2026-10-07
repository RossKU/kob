//! The `kob-executor match` / `keep` loop.
//!
//! One [`Runner::step`] per tick (default 1 s, `docs/spec/matcher.md` §3):
//!
//! 1. node: synced? virtual DAA score; acceptance of our transactions from the VSPC v2 cursor
//!    (reorgs roll back to pending, final after `final_depth` DAA);
//! 2. book: a fresh snapshot from the [`BookSource`] (the indexer's [`OrderBookView`]);
//! 3. funding: the operator's P2PK UTXOs from the node, minus everything pending transactions spend;
//! 4. matcher tick (plans, chains, engine-validated transactions) and keeper tick;
//! 5. submission through the [`Tracker`]: benign double spends back off, nothing is replaced
//!    blindly, chained steps are only sent after their parent was accepted into the mempool.
//!
//! The kill switch is a file (`--pause-file`): while it exists nothing is built or submitted,
//! acceptance tracking continues.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kob_protocol::tx::{KeyUtxo, Utxo};

use super::book::{outpoint, utc_now, Clock, CovId, ListedOrder, MemoryBook, OrderBookView};
use super::engine::{tick as matcher_tick, EngineConfig, TickInput};
use super::family::Families;
use super::node::{NodeApi, RpcError, SubmitOutcome};
use super::submit::{Tracked, Tracker, TrackerConfig};
use super::wallet::{address_of, p2pk_spk_string, Signer};
use crate::keepers::{tick as keeper_tick, KeeperConfig, KeeperInput};

/// Where the book comes from each tick.
pub trait BookSource: Send {
    /// A consistent snapshot of the listed orders and the operator's tokens.
    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook>;

    /// `Err(why)` while the source lags the chain (the indexer after a restart, catching up, or cut off from the node): the
    /// runner plans nothing (matcher, keepers, maintenance) and keeps tracking acceptance. The default: always ready.
    fn ready(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called with the id of a transaction just before it is submitted, so a source that follows the
    /// chain itself (the indexer) sees its acceptance even if a block arrives within milliseconds.
    fn watch(&mut self, _txid: [u8; 32]) {}

    /// Where the selected chain accepts each of `tracked` now (transaction id to accepting chain block
    /// and its DAA score; absent: not accepted), when the source follows the chain itself. `None`: the
    /// source does not, and the runner follows acceptance through the node (`getVirtualChainFromBlockV2`).
    /// In-process the indexer answers, so there is exactly one VSPC follower and one truth about which
    /// transactions are accepted: the one the book was built from.
    fn acceptance(&mut self, _tracked: &[[u8; 32]]) -> Option<BTreeMap<[u8; 32], (String, u64)>> {
        None
    }

    /// Live orders the indexer proved but does not LIST (a token outside the allowlist, an exit of an unlisted entry, a carrier
    /// below the listing minimum, ...): never matched, but the keepers kill, refund and close them when that pays (their
    /// makers would otherwise wait for nobody; the maker's own cancel works either way). The default: none.
    fn keeper_only_orders(&mut self) -> Vec<ListedOrder> {
        vec![]
    }

    /// The operator's own key-owned token UTXOs (accepted, with their programs) for the maintenance jobs
    /// ([`crate::maintenance`]). The default: none (no maintenance).
    fn operator_tokens(&mut self, _operator: &[u8; 32]) -> Vec<crate::maintenance::OwnToken> {
        vec![]
    }

    /// The tick's outcome for orders a token program may refuse to move: `flagged` (id, reason) failed their engine
    /// pre-simulation at the token program, `cleared` passed it. A source that stores the flag ("possibly frozen") keeps it
    /// off the books; the default ignores both.
    fn report_frozen(&mut self, _daa: u64, _flagged: &[(CovId, String)], _cleared: &[CovId]) {}

    /// Extra Prometheus lines for the metrics file (the in-process indexer's follower state). The default: none.
    fn metrics(&mut self) -> String {
        String::new()
    }
}

/// DAA scores (10 per second) an order whose planning panicked, or that the builder refused on its own, stays out of the
/// book: one hour.
pub const QUARANTINE_DAA: u64 = 36_000;

/// DAA scores (10 per second) before an order flagged possibly frozen is probed again: 5 minutes.
pub const FROZEN_RECHECK_DAA: u64 = 3_000;

/// DAA scores (10 per second) a keeper job that failed to build stays out of the keepers' queue the first time: 1 minute,
/// doubling with every further failure of the same order outpoint up to [`KEEPER_FAILED_MAX_DAA`].
pub const KEEPER_FAILED_RECHECK_DAA: u64 = 600;

/// Longest keeper backoff of an order outpoint whose jobs keep failing: one hour.
pub const KEEPER_FAILED_MAX_DAA: u64 = 36_000;

/// Any [`OrderBookView`] (the indexer in-process) is a book source.
pub struct ViewSource<V: OrderBookView + Send>(pub V);

impl<V: OrderBookView + Send> BookSource for ViewSource<V> {
    fn snapshot(&mut self, operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        Ok(MemoryBook { daa_score: self.0.daa_score(), orders: self.0.orders(), wallet_tokens: self.0.wallet_tokens(operator) })
    }
}

/// A JSON snapshot file ([`MemoryBook`]), re-read when it changes (written by an indexer export).
pub struct FileSource {
    pub path: PathBuf,
    cached: Option<(SystemTime, MemoryBook)>,
}

impl FileSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileSource { path: path.into(), cached: None }
    }
}

/// Reads a snapshot file.
pub fn read_snapshot(path: &Path) -> anyhow::Result<MemoryBook> {
    let s = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

impl BookSource for FileSource {
    fn snapshot(&mut self, _operator: &[u8; 32]) -> anyhow::Result<MemoryBook> {
        let m = std::fs::metadata(&self.path)?.modified()?;
        if let Some((t, b)) = &self.cached {
            if *t == m {
                return Ok(b.clone());
            }
        }
        let b = read_snapshot(&self.path)?;
        self.cached = Some((m, b.clone()));
        Ok(b)
    }
}

/// Roles of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Roles {
    pub matcher: bool,
    pub keeper: bool,
}

/// Loop settings.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub network: String,
    pub roles: Roles,
    pub engine: EngineConfig,
    pub keeper: KeeperConfig,
    /// The operator's own tokens: merge and sell (`crate::maintenance`), with the matcher role.
    pub maintenance: crate::maintenance::MaintenanceConfig,
    pub tracker: TrackerConfig,
    /// Build and validate, never submit.
    pub dry_run: bool,
    pub tick: Duration,
    /// Kill switch: while this file exists nothing is built or submitted.
    pub pause_file: Option<PathBuf>,
    /// Prometheus textfile-collector output (`*.prom`).
    pub metrics_file: Option<PathBuf>,
    /// Alert threshold: funding below this many sompi is reported (`kob_operator_low_funds`).
    pub low_funds: u64,
    /// Plan nothing while the book is more than this many DAA behind the node (a lagging indexer would
    /// offer spent orders). Acceptance tracking continues. `None`: no check (snapshot files).
    pub max_book_lag: Option<u64>,
    /// The fee policy (`crate::fee`): each step that plans reads the node's fee estimate (at most every `refresh_ms`) and
    /// sets the tick's rates of the matcher, the keepers and the maintenance jobs. Their configured `fee_rate`s stay the
    /// lowest rate each pays.
    pub fee_policy: crate::fee::FeePolicy,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            network: "testnet-10".into(),
            roles: Roles { matcher: true, keeper: false },
            engine: EngineConfig::default(),
            keeper: KeeperConfig::default(),
            maintenance: crate::maintenance::MaintenanceConfig::default(),
            tracker: TrackerConfig::default(),
            dry_run: false,
            tick: Duration::from_secs(1),
            pause_file: None,
            metrics_file: None,
            low_funds: 100 * 100_000_000,
            max_book_lag: None,
            fee_policy: crate::fee::FeePolicy::default(),
        }
    }
}

/// What one step did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepReport {
    pub daa: u64,
    pub synced: bool,
    pub paused: bool,
    /// The book was too far behind the node to plan against (`RunConfig::max_book_lag`).
    pub book_stale: bool,
    /// DAA score the book was synced to (0: no book read).
    pub book_daa: u64,
    /// The book source was not ready (`BookSource::ready`): why. Nothing was planned.
    pub not_ready: Option<String>,
    /// (kind, txid, operator profit) of every transaction built this step.
    pub built: Vec<(String, [u8; 32], i64)>,
    pub submitted: Vec<[u8; 32]>,
    pub conflicts: Vec<[u8; 32]>,
    pub rejected: Vec<([u8; 32], SubmitOutcome)>,
    /// Transactions whose submit got no verdict (tracked as pending, resent).
    pub unknown: Vec<[u8; 32]>,
    pub skipped: Vec<String>,
    /// Orders left out of the matcher's book this step (numbers refused by the sanity gate, planning panicked, or the builder
    /// refused the order).
    pub quarantined: usize,
    pub accepted: usize,
    pub finalized: usize,
    pub rolled_back: usize,
    /// The operator's spendable funding read this step (`None`: not read, nothing was planned).
    pub funding: Option<u64>,
}

/// The matcher / keeper process.
pub struct Runner<N: NodeApi, S: BookSource> {
    pub node: N,
    pub source: S,
    pub signer: Box<dyn Signer>,
    pub families: Families,
    pub tracker: Tracker,
    pub cfg: RunConfig,
    pub last: StepReport,
    pub steps: u64,
    /// Orders flagged possibly frozen and the DAA score before which the matcher does not probe them again.
    pub frozen_until: BTreeMap<CovId, u64>,
    /// Orders whose planning panicked (the matcher's last line of defence) or that the builder refused on their own, and the
    /// DAA score before which they stay out of the book: one malformed order costs a skipped order, never the process or the
    /// other books.
    pub quarantined_until: BTreeMap<CovId, u64>,
    /// Refunds found unprofitable at their fixed tip, by (order, order outpoint): not built again.
    pub keeper_unprofitable: BTreeSet<(CovId, super::book::Outpoint)>,
    /// The keepers' floor fee rate when the entries of `keeper_unprofitable` were found: a lower floor forgets them (a refund
    /// that did not pay at the old floor may pay at the new one).
    pub keeper_unprofitable_floor: u64,
    /// Keeper jobs that failed to build, by order outpoint: (DAA before which it is not built again, failures so far).
    pub keeper_failed: BTreeMap<super::book::Outpoint, (u64, u32)>,
    /// Diagnostics and tests: every transaction submitted (or built, in a dry run) is also pushed here.
    /// `None` (the default) keeps nothing.
    pub capture: Option<Vec<kob_protocol::tx::SignedTx>>,
    /// Outpoints that transactions of another part of the process spend while in flight (the x402 facilitator's swap-and-pay
    /// settlements, signed by their payers over the same book): the batches plan around them like around the runner's own
    /// pending transactions.
    pub reservations: Option<Reservations>,
    /// Why the book source is not ready, while the runner waits for it (logged once per wait).
    pub waiting: Option<String>,
    /// The funding of the last step that read it (the metrics keep it while a step reads none).
    pub last_funding: Option<u64>,
    /// The node's fee estimate between steps (`crate::fee`).
    pub fee_state: crate::fee::FeeState,
    /// The rates of the last step that planned (the floor before the first).
    pub fee_rates: crate::fee::FeeRates,
    /// The matcher's configured rate (`engine.planner.fee_rate` at start): the lowest rate a batch is planned at.
    pub plan_base_rate: u64,
    /// Where the rates are published for the rest of the process (the x402 facilitator of `kob-executor run`).
    pub fee_board: Option<crate::fee::FeeBoard>,
}

/// A source of reserved outpoints ([`Runner::reservations`]).
pub type Reservations = std::sync::Arc<dyn Fn() -> BTreeSet<super::book::Outpoint> + Send + Sync>;

fn rpc_json(signed: &kob_protocol::tx::SignedTx) -> anyhow::Result<serde_json::Value> {
    let (tx, _) = signed.tx.to_tx()?;
    Ok(kob_protocol::issue::rpc_transaction_json(&tx))
}

/// Orders whose current order or custody outpoint a pending transaction spends.
fn busy_orders(orders: &[ListedOrder], spent: &BTreeSet<super::book::Outpoint>) -> BTreeSet<CovId> {
    orders
        .iter()
        .filter(|o| {
            spent.contains(&outpoint(&o.order.utxo))
                || o.custody.iter().chain(o.custody_b.iter()).any(|c| spent.contains(&outpoint(&c.utxo)))
        })
        .map(|o| o.id())
        .collect()
}

impl<N: NodeApi, S: BookSource> Runner<N, S> {
    pub fn new(node: N, source: S, signer: Box<dyn Signer>, cfg: RunConfig) -> Self {
        let tracker = Tracker::new(cfg.tracker.clone());
        let fee_rates = cfg.fee_policy.rates(None);
        let plan_base_rate = cfg.engine.planner.fee_rate;
        Runner {
            fee_state: crate::fee::FeeState::default(),
            fee_rates,
            plan_base_rate,
            fee_board: None,
            node,
            source,
            signer,
            families: Families::default(),
            tracker,
            cfg,
            last: StepReport::default(),
            steps: 0,
            frozen_until: BTreeMap::new(),
            quarantined_until: BTreeMap::new(),
            keeper_unprofitable: BTreeSet::new(),
            keeper_unprofitable_floor: 0,
            keeper_failed: BTreeMap::new(),
            capture: None,
            reservations: None,
            waiting: None,
            last_funding: None,
        }
    }

    /// Outpoints no plan may spend: the tracker's pending and unfinalized transactions and the reservations.
    fn reserved_or_spent(&self) -> BTreeSet<super::book::Outpoint> {
        let mut s = self.tracker.spent_outpoints();
        if let Some(r) = &self.reservations {
            s.extend(r());
        }
        s
    }

    fn paused(&self) -> bool {
        self.cfg.pause_file.as_ref().is_some_and(|p| p.exists())
    }

    /// Reports a tracker step and resends what a reorg moved back to pending.
    async fn on_events(&mut self, ev: super::submit::TrackerEvents, rep: &mut StepReport) {
        rep.accepted = ev.accepted.len();
        rep.finalized = ev.finalized.len();
        rep.rolled_back = ev.rolled_back.len();
        for t in &ev.finalized {
            tracing::info!(txid = %kob_protocol::json::to_hex(&t.txid), kind = %t.kind, profit = t.profit, "final");
        }
        for id in &ev.rolled_back {
            tracing::warn!(txid = %kob_protocol::json::to_hex(id), "rolled back by a reorg; re-checking");
        }
        // Idempotent resend of rolled-back transactions still possible. Not while the kill switch is on: `--pause-file` means
        // nothing leaves the process.
        if !self.cfg.dry_run && !self.paused() {
            let resend: Vec<Tracked> = ev.rolled_back.iter().filter_map(|id| self.tracker.txs.get(id).cloned()).collect();
            for t in resend {
                let _ = self.node.submit(t.rpc_tx.clone()).await;
            }
        }
    }

    async fn follow_chain(&mut self, daa: u64, rep: &mut StepReport) -> Result<(), RpcError> {
        // The book source follows the chain itself (the indexer): one follower, one truth.
        let tracked: Vec<[u8; 32]> = self.tracker.txs.keys().copied().collect();
        if let Some(view) = self.source.acceptance(&tracked) {
            let ev = self.tracker.observe(&view, daa);
            self.on_events(ev, rep).await;
            return Ok(());
        }
        let cursor = match &self.tracker.cursor {
            Some(c) => c.clone(),
            None => {
                let sink = self.node.dag_info().await?.sink;
                self.tracker.cursor = Some(sink.clone());
                sink
            }
        };
        match self.node.chain_from(&cursor).await {
            Ok(u) => {
                let ev = self.tracker.apply(&u, daa);
                self.on_events(ev, rep).await;
                Ok(())
            }
            Err(RpcError::Node(m)) => {
                // The cursor block is unknown (pruned or reorged away): restart from the sink.
                tracing::warn!(error = %m, "VSPC cursor lost; restarting from the sink");
                self.tracker.cursor = None;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    async fn funding(&self, spent: &BTreeSet<super::book::Outpoint>) -> Result<Vec<KeyUtxo>, RpcError> {
        let pk = self.signer.pubkey();
        let addr = address_of(&pk, &self.cfg.network).to_string();
        let spk = p2pk_spk_string(&pk);
        let utxos = self.node.utxos_by_addresses(&[addr]).await?;
        Ok(utxos
            .into_iter()
            .filter(|u| u.covenant_id.is_none() && u.script_public_key == spk && !spent.contains(&(u.transaction_id, u.index)))
            .map(|u| KeyUtxo {
                utxo: Utxo {
                    transaction_id: u.transaction_id,
                    index: u.index,
                    amount: u.amount,
                    block_daa_score: u.block_daa_score,
                    covenant_id: None,
                },
                pubkey: pk,
            })
            .collect())
    }

    /// The fee rates of a step that plans (`crate::fee`): the node's estimate is read at most every `refresh_ms`, the last good
    /// one used up to `max_age_ms`, then the floor. Sets the rates of the matcher, the keepers and the maintenance jobs.
    async fn refresh_fees(&mut self) {
        let policy = self.cfg.fee_policy.clone();
        let now = std::time::Instant::now();
        if self.fee_state.due(&policy, now) {
            let r = self.node.fee_estimate().await.map_err(|e| e.to_string());
            self.fee_state.record(now, r);
        }
        let rates = self.fee_state.rates(&policy, now);
        if rates != self.fee_rates {
            tracing::info!(
                high = rates.high,
                normal = rates.normal,
                low = rates.low,
                floor = rates.floor,
                estimated = rates.estimated,
                "fee rates (sompi per gram)"
            );
        }
        self.fee_rates = rates;
        self.cfg.engine.planner.fee_rate = rates.rate(crate::fee::Urgency::Normal, self.plan_base_rate);
        self.cfg.engine.fees = rates;
        self.cfg.keeper.fees = rates;
        self.cfg.maintenance.fees = rates;
        if let Some(b) = &self.fee_board {
            *b.lock().unwrap_or_else(|e| e.into_inner()) = rates;
        }
    }

    async fn send(&mut self, t: Tracked, daa: u64, rep: &mut StepReport) {
        let txid = t.txid;
        let kind = t.kind.clone();
        let (fee, fee_rate) = (t.fee, t.fee_rate);
        self.source.watch(txid);
        let outcome = self.tracker.submit(&self.node, t, daa).await;
        match outcome {
            SubmitOutcome::Accepted | SubmitOutcome::AlreadyKnown => {
                tracing::info!(txid = %kob_protocol::json::to_hex(&txid), kind = %kind, fee, fee_rate, "submitted");
                rep.submitted.push(txid);
            }
            SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => {
                tracing::info!(txid = %kob_protocol::json::to_hex(&txid), ?outcome, "lost a race; backing off");
                rep.conflicts.push(txid);
            }
            SubmitOutcome::Unknown => {
                tracing::warn!(txid = %kob_protocol::json::to_hex(&txid), "no verdict from the node; tracking it as pending and resending");
                rep.unknown.push(txid);
            }
            other => {
                tracing::warn!(txid = %kob_protocol::json::to_hex(&txid), ?other, "rejected");
                rep.rejected.push((txid, other));
            }
        }
    }

    /// One tick.
    pub async fn step(&mut self) -> anyhow::Result<StepReport> {
        let mut rep = StepReport::default();
        let info = self.node.server_info().await?;
        rep.daa = info.virtual_daa_score;
        rep.synced = info.is_synced;
        if info.network_id != self.cfg.network {
            anyhow::bail!("node reports network `{}`, expected `{}`", info.network_id, self.cfg.network);
        }
        if !info.is_synced {
            self.finish(&rep);
            return Ok(rep);
        }
        let daa = info.virtual_daa_score;
        self.follow_chain(daa, &mut rep).await?;
        if self.paused() {
            rep.paused = true;
            self.finish(&rep);
            return Ok(rep);
        }
        // A submit that got no answer may have reached the node: settle the doubt before planning anything over its inputs.
        if !self.cfg.dry_run {
            for (id, outcome) in self.tracker.resubmit_unconfirmed(&self.node, daa).await {
                tracing::info!(txid = %kob_protocol::json::to_hex(&id), ?outcome, "resent a submit that had no verdict");
                match outcome {
                    SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => rep.conflicts.push(id),
                    SubmitOutcome::Rejected => rep.rejected.push((id, outcome)),
                    _ => {}
                }
            }
        }
        // Nothing is planned over a store that lags the chain: after a restart the store is where the last run left it (seconds
        // to hours behind), so the orders and the operator's tokens it lists may be spent already (the soak's restarts lost
        // maintenance sells to `MissingInput` that way). Acceptance tracking above continues.
        if let Err(why) = self.source.ready() {
            if self.waiting.is_none() {
                tracing::info!(%why, "waiting for the indexer to catch up; planning nothing");
            }
            self.waiting = Some(why.clone());
            rep.not_ready = Some(why);
            self.finish(&rep);
            return Ok(rep);
        }
        if let Some(why) = self.waiting.take() {
            tracing::info!(was = %why, "the indexer caught up; planning");
        }
        let operator = self.signer.pubkey();
        let book = self.source.snapshot(&operator)?;
        rep.book_daa = book.daa_score;
        if self.cfg.max_book_lag.is_some_and(|lag| book.daa_score.saturating_add(lag) < daa) {
            tracing::warn!(book_daa = book.daa_score, node_daa = daa, "the book lags the node; planning nothing until it catches up");
            rep.book_stale = true;
            self.finish(&rep);
            return Ok(rep);
        }
        self.refresh_fees().await;
        let mut spent = self.reserved_or_spent();
        let funding = self.funding(&spent).await?;
        rep.funding = Some(funding.iter().map(|k| k.utxo.amount).sum());
        self.last_funding = rep.funding;
        let clock = Clock { daa, utc: utc_now() };
        let mut excluded = self.tracker.backed_off(daa);
        excluded.extend(busy_orders(&book.orders, &spent));
        self.frozen_until.retain(|_, until| *until > daa);
        excluded.extend(self.frozen_until.keys().copied());
        self.quarantined_until.retain(|_, until| *until > daa);
        excluded.extend(self.quarantined_until.keys().copied());

        if self.cfg.roles.matcher {
            let inp = TickInput { orders: book.orders.clone(), clock, funding: funding.clone(), excluded: excluded.clone() };
            let report = matcher_tick(&inp, &self.cfg.engine, &self.families, self.signer.as_ref());
            for (id, why) in &report.quarantined {
                tracing::error!(order = %kob_protocol::json::to_hex(id), %why, "order quarantined: left out of the matcher's book");
                if why == super::engine::PLANNING_PANICKED || why.starts_with(super::engine::REFUSED_BY_THE_BUILDER) {
                    self.quarantined_until.insert(*id, daa + QUARANTINE_DAA);
                }
                rep.quarantined += 1;
            }
            // Orders a token program refuses in the engine pre-simulation are flagged and probed again later; those whose
            // pre-simulation passed lose the flag.
            let mut flagged: Vec<(CovId, String)> = vec![];
            for s in &report.suspects {
                if !flagged.iter().any(|(id, _)| *id == s.id) {
                    flagged.push((s.id, s.reason.clone()));
                }
            }
            if !flagged.is_empty() || !report.cleared.is_empty() {
                for (id, why) in &flagged {
                    tracing::warn!(order = %kob_protocol::json::to_hex(id), %why, "order possibly frozen: left out of the books");
                    self.frozen_until.insert(*id, daa + FROZEN_RECHECK_DAA);
                }
                let cleared: Vec<CovId> = report.cleared.iter().filter(|c| !flagged.iter().any(|(id, _)| id == *c)).copied().collect();
                self.source.report_frozen(daa, &flagged, &cleared);
            }
            for (k, why) in &report.skipped {
                rep.skipped.push(format!("{}: {why}", kob_protocol::json::to_hex(&k.token)));
            }
            for p in report.prepared {
                let kind = "match";
                if !p.plan.updates.is_empty() || p.plan.triggered() > 0 {
                    tracing::info!(
                        txid = %kob_protocol::json::to_hex(&p.txid()),
                        triggered = p.plan.triggered(),
                        updates = p.plan.updates.len(),
                        "the batch triggers or arms stops"
                    );
                }
                for k in &p.plan.kept {
                    tracing::info!(
                        txid = %kob_protocol::json::to_hex(&p.txid()),
                        token = %kob_protocol::json::to_hex(&k.token),
                        amount = k.amount,
                        value = k.value,
                        "the batch keeps a token surplus as inventory"
                    );
                }
                rep.built.push((kind.into(), p.txid(), p.accounting.profit));
                if let Some(c) = self.capture.as_mut() {
                    c.push(p.signed.clone());
                }
                if self.cfg.dry_run {
                    continue;
                }
                let t = Tracked {
                    txid: p.txid(),
                    spends: p.signed.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect(),
                    orders: p.plan.spent_ids(),
                    parent: p.parent,
                    submitted_daa: daa,
                    rpc_tx: rpc_json(&p.signed)?,
                    accepted: None,
                    kind: kind.into(),
                    profit: p.accounting.profit,
                    fee: p.lowered.built.fee.fee,
                    fee_rate: p.lowered.built.fee.fee_rate,
                    unconfirmed: false,
                };
                self.send(t, daa, &mut rep).await;
            }
            spent = self.reserved_or_spent();
        }

        if self.cfg.roles.keeper {
            let funding: Vec<KeyUtxo> = funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect();
            // the listed orders plus the proven unlisted ones (kill / refund / close only: the keepers never match)
            let mut orders = book.orders.clone();
            let listed: BTreeSet<CovId> = orders.iter().map(|o| o.id()).collect();
            orders.extend(self.source.keeper_only_orders().into_iter().filter(|o| !listed.contains(&o.id())));
            let mut excluded = self.tracker.backed_off(daa);
            excluded.extend(busy_orders(&orders, &spent));
            // a token program refused to move these in the matcher's pre-simulation: a refund would fail the same way
            excluded.extend(self.frozen_until.keys().copied());
            // forget the orders that moved on
            let live: BTreeSet<(CovId, super::book::Outpoint)> = orders.iter().map(|o| (o.id(), outpoint(&o.order.utxo))).collect();
            let live_outpoints: BTreeSet<super::book::Outpoint> = live.iter().map(|(_, op)| *op).collect();
            self.keeper_unprofitable.retain(|k| live.contains(k));
            let floor = self.cfg.keeper.fees.floor_of(self.cfg.keeper.fee_rate);
            if floor < self.keeper_unprofitable_floor {
                self.keeper_unprofitable.clear();
            }
            self.keeper_unprofitable_floor = floor;
            self.keeper_failed.retain(|op, _| live_outpoints.contains(op));
            let mut excluded_outpoints = spent;
            excluded_outpoints.extend(self.keeper_failed.iter().filter(|(_, (until, _))| *until > daa).map(|(op, _)| *op));
            let inp = KeeperInput {
                orders,
                clock,
                funding,
                excluded,
                excluded_outpoints,
                known_unprofitable: self.keeper_unprofitable.clone(),
            };
            let report = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                keeper_tick(&inp, &self.cfg.keeper, self.signer.as_ref())
            })) {
                Ok(r) => r,
                Err(_) => {
                    tracing::error!("keeper tick panicked; skipped (the process lives)");
                    rep.skipped.push("keeper: tick panicked".into());
                    crate::keepers::KeeperReport::default()
                }
            };
            for (id, why) in &report.skipped {
                rep.skipped.push(format!("{}: {why}", kob_protocol::json::to_hex(id)));
            }
            self.keeper_unprofitable.extend(report.unprofitable.iter().copied());
            // bounded retry of jobs that failed to build: a growing backoff per order outpoint (a new outpoint starts over)
            for (_, op) in &report.failed {
                let e = self.keeper_failed.entry(*op).or_insert((0, 0));
                e.1 = e.1.saturating_add(1);
                let wait = KEEPER_FAILED_RECHECK_DAA.saturating_mul(1u64 << (e.1 - 1).min(16)).min(KEEPER_FAILED_MAX_DAA);
                e.0 = daa.saturating_add(wait);
            }
            for id in &report.foreign_strays {
                tracing::debug!(order = %kob_protocol::json::to_hex(id), "order holds strays (only its maker's cancel moves them)");
            }
            for j in report.jobs {
                rep.built.push((j.kind.name().into(), j.signed.tx.id, j.profit));
                if let Some(c) = self.capture.as_mut() {
                    c.push(j.signed.clone());
                }
                if self.cfg.dry_run {
                    continue;
                }
                let t = Tracked {
                    txid: j.signed.tx.id,
                    spends: j.spends.clone(),
                    orders: [j.order].into_iter().collect(),
                    parent: None,
                    submitted_daa: daa,
                    rpc_tx: rpc_json(&j.signed)?,
                    accepted: None,
                    kind: j.kind.name().into(),
                    profit: j.profit,
                    fee: j.built.fee.fee,
                    fee_rate: j.built.fee.fee_rate,
                    unconfirmed: false,
                };
                self.send(t, daa, &mut rep).await;
            }
        }

        if self.cfg.roles.matcher && self.cfg.maintenance.enabled {
            self.maintenance(&book.orders, clock, &funding, daa, &mut rep).await?;
        }
        self.finish(&rep);
        Ok(rep)
    }

    /// The maintenance jobs (`crate::maintenance`): the operator's token UTXOs merged or sold, after the matcher's and the
    /// keepers' transactions of the tick (whose inputs are left alone).
    async fn maintenance(
        &mut self,
        orders: &[ListedOrder],
        clock: Clock,
        funding: &[KeyUtxo],
        daa: u64,
        rep: &mut StepReport,
    ) -> anyhow::Result<()> {
        let operator = self.signer.pubkey();
        let spent = self.reserved_or_spent();
        let tokens: Vec<crate::maintenance::OwnToken> =
            self.source.operator_tokens(&operator).into_iter().filter(|t| !spent.contains(&outpoint(&t.token.utxo))).collect();
        if tokens.is_empty() {
            return Ok(());
        }
        let mut excluded = self.tracker.backed_off(daa);
        excluded.extend(busy_orders(orders, &spent));
        excluded.extend(self.frozen_until.keys().copied());
        excluded.extend(self.quarantined_until.keys().copied());
        let inp = crate::maintenance::MaintenanceInput {
            tokens,
            orders: orders.to_vec(),
            clock,
            funding: funding.iter().filter(|k| !spent.contains(&outpoint(&k.utxo))).cloned().collect(),
            excluded,
            excluded_outpoints: spent,
        };
        let report = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::maintenance::tick(&inp, &self.cfg.maintenance, self.signer.as_ref())
        })) {
            Ok(r) => r,
            Err(_) => {
                tracing::error!("maintenance tick panicked; skipped (the process lives)");
                rep.skipped.push("maintenance: tick panicked".into());
                crate::maintenance::MaintReport::default()
            }
        };
        for (token, why) in &report.skipped {
            tracing::debug!(token = %kob_protocol::json::to_hex(token), %why, "maintenance job not built");
        }
        for j in report.jobs {
            tracing::info!(
                kind = j.kind.name(),
                token = %kob_protocol::json::to_hex(&j.token),
                txid = %kob_protocol::json::to_hex(&j.signed.tx.id),
                released = j.released,
                profit = j.profit,
                "maintenance: the operator's token UTXOs"
            );
            rep.built.push((j.kind.name().into(), j.signed.tx.id, j.profit));
            if let Some(c) = self.capture.as_mut() {
                c.push(j.signed.clone());
            }
            if self.cfg.dry_run {
                continue;
            }
            let t = Tracked {
                txid: j.signed.tx.id,
                spends: j.spends.clone(),
                orders: j.order.into_iter().collect(),
                parent: None,
                submitted_daa: daa,
                rpc_tx: rpc_json(&j.signed)?,
                accepted: None,
                kind: j.kind.name().into(),
                profit: j.profit,
                fee: j.built.fee.fee,
                fee_rate: j.built.fee.fee_rate,
                unconfirmed: false,
            };
            self.send(t, daa, rep).await;
        }
        Ok(())
    }

    fn finish(&mut self, rep: &StepReport) {
        self.steps += 1;
        self.last = rep.clone();
        if let Some(p) = self.cfg.metrics_file.clone() {
            if let Err(e) = write_metrics(&p, self, rep) {
                tracing::warn!(error = %e, "cannot write metrics");
            }
        }
    }

    /// Runs until `shutdown` resolves.
    pub async fn run(&mut self, shutdown: impl std::future::Future<Output = ()>) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        loop {
            match self.step().await {
                Ok(r) => {
                    if !r.built.is_empty() || !r.skipped.is_empty() {
                        tracing::info!(
                            daa = r.daa,
                            built = r.built.len(),
                            submitted = r.submitted.len(),
                            conflicts = r.conflicts.len(),
                            skipped = r.skipped.len(),
                            "tick"
                        );
                        // why a book or job was skipped (an engine rejection, unprofitable, too large): the operator's only
                        // trace of a batch that keeps failing (the soak's cross-limit budget finding needed it)
                        for why in &r.skipped {
                            tracing::info!(why = %why, "skipped");
                        }
                    }
                    // only a funding actually read is judged (a step that planned nothing read none: not `0`)
                    if let Some(f) = r.funding.filter(|f| *f < self.cfg.low_funds && !self.cfg.dry_run) {
                        tracing::warn!(funding = f, "operator funds are low");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "tick failed"),
            }
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                _ = tokio::time::sleep(self.cfg.tick) => {}
            }
        }
    }
}

/// Prometheus textfile-collector metrics.
pub fn metrics_text<N: NodeApi, S: BookSource>(r: &Runner<N, S>, rep: &StepReport) -> String {
    let t = &r.tracker;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut s = String::new();
    let mut g = |name: &str, help: &str, v: String| {
        s.push_str(&format!("# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}\n"));
    };
    g("kob_matcher_last_step_seconds", "Unix time of the last completed step.", now.to_string());
    g("kob_matcher_daa_score", "Node virtual DAA score at the last step.", rep.daa.to_string());
    g("kob_matcher_node_synced", "1 when the node reports synced.", (rep.synced as u8).to_string());
    g("kob_matcher_paused", "1 while the kill-switch file exists.", (rep.paused as u8).to_string());
    g("kob_matcher_book_stale", "1 when the book lagged the node and nothing was planned.", (rep.book_stale as u8).to_string());
    g(
        "kob_matcher_book_lag_daa",
        "DAA the book is behind the node at the last step.",
        if rep.book_daa == 0 { 0 } else { rep.daa.saturating_sub(rep.book_daa) }.to_string(),
    );
    g("kob_matcher_submitted_total", "Transactions accepted into the mempool.", t.submitted.to_string());
    g("kob_matcher_conflicts_total", "Benign double spends / lost races.", t.conflicts.to_string());
    g("kob_matcher_rejected_total", "Other rejections (investigate).", t.rejected.to_string());
    g(
        "kob_matcher_unknown_submits_total",
        "Submits without a verdict from the node (tracked as pending, resent).",
        t.unknown.to_string(),
    );
    g("kob_matcher_finalized_total", "Transactions final (accepted + depth).", t.finalized_count.to_string());
    g("kob_matcher_finalized_profit_sompi", "Operator profit of final transactions.", t.finalized_profit.to_string());
    g("kob_matcher_pending_txs", "Submitted, not yet accepted.", t.pending().to_string());
    g("kob_matcher_tracked_txs", "Pending or accepted but not final.", t.txs.len().to_string());
    // the last funding read (a step that waited read none); unknown before the first read: no alert
    let funding = rep.funding.or(r.last_funding);
    g("kob_matcher_funding_sompi", "Spendable operator P2PK balance.", funding.unwrap_or(0).to_string());
    g(
        "kob_operator_low_funds",
        "1 when funding is below the alert threshold.",
        (funding.is_some_and(|f| f < r.cfg.low_funds) as u8).to_string(),
    );
    g(
        "kob_matcher_waiting_for_indexer",
        "1 when the indexer lagged the chain and nothing was planned.",
        (rep.not_ready.is_some() as u8).to_string(),
    );
    g("kob_matcher_skipped_books", "Books or jobs skipped in the last step.", rep.skipped.len().to_string());
    let f = &r.fee_rates;
    g("kob_fee_rate_high", "Fee rate of urgent transactions (sompi per gram).", f.high.to_string());
    g("kob_fee_rate_normal", "Fee rate of normal transactions (sompi per gram).", f.normal.to_string());
    g("kob_fee_rate_low", "Fee rate of housekeeping transactions (sompi per gram).", f.low.to_string());
    g("kob_fee_estimated", "1 when the rates come from the node's fee estimate, 0 at the floor.", (f.estimated as u8).to_string());
    g("kob_fee_estimate_failures_total", "Fee estimate reads that failed.", r.fee_state.failures.to_string());
    s
}

fn write_metrics<N: NodeApi, S: BookSource>(p: &Path, r: &mut Runner<N, S>, rep: &StepReport) -> std::io::Result<()> {
    let tmp = p.with_extension("prom.tmp");
    let extra = r.source.metrics();
    std::fs::write(&tmp, metrics_text(r, rep) + &extra)?;
    std::fs::rename(tmp, p)
}
