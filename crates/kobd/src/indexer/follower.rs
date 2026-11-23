//! The VSPC v2 follower, which polls the node from the stored cursor and commits atomically.
//!
//! # Failure policy
//!
//! The cursor is never advanced on any of these:
//! - A transport error, a timeout, or `consensus is currently in a transitional ibd state`: back
//!   off and retry.
//! - A block of the batch to prove at left the selected parent chain (a tip reorg): fetch the
//!   batch again at once, a few blue scores short of the sink, and prove it anew. The node is not
//!   blamed and the state is not `node_unavailable`.
//! - `cannot find header`, which means the node does not know the cursor: walk the stored chain
//!   back to the newest block the node still knows, revert everything above it, and continue from
//!   there. If none is known, or the node says `the queried hash does not have retention root on
//!   its chain`, the downtime exceeded the retention of the node. The state is `gap`, there is no
//!   further progress, and the operator bootstraps the store (see the docs).
//! - The rewind and the gap above wait while the node is not synced (a reset or
//!   re-syncing node may not know the cursor yet): the state is `node_unavailable` until it is,
//!   and then the cursor is asked again. A cursor it knows again is followed on with no gap.
//! - With a cross-checking node (the public pair), `cannot find header` and `retention root` count
//!   only when that node confirms them (`ChainSource::confirm_unknown_cursor`). Otherwise nothing
//!   is rewound or halted, and the follower retries.
//! - A `removed` list that contradicts the stored chain: state `gap`. This is a different node or
//!   a bug, and the follower never guesses. It waits like the rewind above while the node is not
//!   synced, and once it is, the batch is fetched again: only a contradiction that holds then halts.
//! - A record-log write that failed, for example on a full disk or an I/O error. The append is
//!   rolled back. The state is `stopped`, and nothing more is appended until the operator
//!   restarts.
//! - A chain authority that says it lacks a call the follower needs (`getVirtualChainFromBlockV2`,
//!   or `getSeqCommitLaneProof` with chain proofs on): state `stopped` with the reason, checked
//!   once per authority before its first step ([`ChainSource::check_capabilities`]). One that only
//!   closes the connection on it is warned about and checked again, never stopped on.
//! - A lane proof the synced node can no longer serve, because the cursor is before its pruning
//!   point (`BlockTooDeep`): state `gap`. No batch from there can be proven, and none is
//!   applied unproven. A recovering follower does not move the store past it.
//!
//! # Batch size
//!
//! Every request is bounded to a window of blue score ahead of the cursor (`rpc::window`). A
//! follower that fell behind under a transaction flood pages through the backlog in batches it can
//! fetch within its timeout, instead of asking for the whole backlog at once. A timed-out batch is
//! split, so the window shrinks. A fast batch grows the window. A request is never repeated
//! unchanged after a timeout.
//!
//! # Parallel fetch
//!
//! With `fetch_parallel` > 1, the follower fetches several upcoming windows at once over separate
//! connections and applies them in chain order ([`super::prefetch`]). It does so while the cursor
//! is more than `prefetch_min_lag_blue` behind the sink and at least a first window of chain
//! blocks remains. Near the sink, and whenever the plan stops matching the chain of the node, it
//! takes the single steps above.
//!
//! A plan ends at the sink. Nothing more is planned once an applied window leaves the cursor
//! within the threshold, or once the chain after the tail of the plan is shorter than a first
//! window. The windows already fetched are applied, and the single steps follow the sink from
//! there.
//!
//! # A failing chain authority
//!
//! A node can stay reachable and answer its small calls while its chain requests (a VSPC batch,
//! the chain hashes, its copy of a window from another node) time out or lose the connection.
//! Once [`FollowerConfig::authority_failures`] of them failed in a row and the cursor has not
//! moved for [`FollowerConfig::authority_stall`] (the local monotonic clock), the follower tells
//! the source ([`ChainSource::authority_failing`]). With several nodes, another node that can lead
//! becomes the chain authority (`rpc::multi`), and the follower asks it at once
//! ([`StepOutcome::Handover`]). With a single node nothing changes: it retries with its backoff.
//!
//! # Progress
//!
//! Whether the chain authority makes progress is decided from two things only: the header time
//! of the cursor's chain block, as it was verified when the block was applied, and the local
//! clock (R4C1-M1, R5C1-M1, R5C1-M2). No field of any answer of the authority (its sink, its
//! virtual DAA score, "more follows", an empty answer, how long it took) and no claim of another
//! node enters it.
//! - The *lag* is the local monotonic clock (as Unix ms) minus that header time.
//! - While the lag is at most [`FollowerConfig::authority_lag_tolerance`] (90 s), the authority
//!   keeps up.
//! - Above it, the authority must gain on real time: over every
//!   [`FollowerConfig::authority_stall`] (180 s) of the follower's own running time, the lag must
//!   fall by at least half of that time (90 s). A *period* begins when the lag rises above the
//!   tolerance and ends once the lag has stayed within it for a whole period; a lag that rises
//!   above it again before then continues the period, so a lag that swings around the tolerance
//!   does not start a new period at each swing. A period that reaches its end without the gain is
//!   the authority's failure: it is handed over as a failing authority
//!   ([`ChainSource::authority_failing`]), and with nothing to hand over to the health says so
//!   (`authority_pace` `failing`: `/v1/health` is not `ok` and not ready).
//! - Once an authority has kept up, the running time it spends with the lag above the
//!   tolerance is limited to [`EXCURSION_BUDGET`] (300 s) within any [`EXCURSION_WINDOW`] (an
//!   hour) of running time; past it, it fails the same way. A lag that swings above the tolerance
//!   and back within each period is not kept up. A stint's catch-up before it first keeps up is
//!   not charged.
//! - A deposal is a strike against the deposed node ([`ChainSource::deposal_confirmed`]: longer
//!   away each time) only once its successor keeps up after serving chain stamped between the
//!   cursor block (plus the tolerance) and the deposal: chain that existed and was held back. A
//!   halted network serves none, so it strikes no one.
//! - The running time is the follower's own: it advances while a step or the pause after it
//!   runs, in ticks of at most [`PAUSE_CAP`]. Whatever a node takes to answer is in it (every wait
//!   on a node is a wait inside a step), the follower's own database work is not, and a gap
//!   longer than [`PAUSE_CAP`] (the process stopped, the host asleep, a commit or a storage error
//!   that took that long) starts the period again from the lag at that moment (R5C1-L1).
//! - A new chain authority ([`ChainSource::authority_node`]: a handover, a failover switch, a
//!   resolver entry on another node) and a restart begin with no period: the new one has one
//!   period to show the gain.
//! - A network that stops making blocks fails every authority the same way: the nodes are handed
//!   over in turn every period until blocks come again, which costs nothing but the handovers.
//!
//! The header time is the proven header's with chain proofs on. With them off it is taken only
//! from a header that hashes to the block's hash (R5C1-L2), and a time not known counts as not
//! keeping up. `/v1/health` reports `lag_seconds` as at least the cursor block's age by the local
//! clock, and `authority_pace` as this rule's verdict.

/// The start of a retry reason that says a tip reorg took a block of the batch off the chain.
pub const TIP_REORG: &str = "the batch left the selected parent chain (a tip reorg)";

/// Fetches of a batch again in one step after a tip reorg ([`TIP_REORG`]).
const TIP_REORG_REFETCHES: u32 = 2;

/// Confirmations (blue score below the sink) a batch fetched again after a tip reorg ends at,
/// about a second at 10 BPS, below where tip reorgs reach.
const TIP_REORG_CONFIRMATIONS: u64 = 10;

/// Steps in a row that gave up on tip reorgs ...
const TIP_REORG_STALL_STEPS: u32 = 5;
/// ... over at least this long, before it counts as a failure of the chain authority (K-M1): a
/// node that answers every lane proof "not in the selected parent chain" stalls the cursor.
const TIP_REORG_STALL: Duration = Duration::from_secs(60);

/// Tip reorgs since the cursor last moved.
#[derive(Debug, Default)]
struct TipReorgs {
    /// Steps in a row that gave up on one.
    steps: u32,
    /// Since the first of them.
    since: Option<std::time::Instant>,
    /// The bound was crossed: the next hand-over check need not wait for `authority_stall`.
    stalled: bool,
}

fn is_tip_reorg(o: &StepOutcome) -> bool {
    matches!(o, StepOutcome::Retry(m) if m.starts_with(TIP_REORG))
}

/// The start of a retry reason that says the node is not synced, so nothing is rewound or halted.
pub const NOT_SYNCED: &str = "the node is not synced";

/// The start of a retry reason that says the synced node knows the cursor it disowned before.
const KNOWN_AGAIN: &str = "the synced node knows the cursor again:";

/// The start of a retry reason that says the node, synced now, is asked again before a halt on a
/// contradiction it gave while not synced.
const ASK_AGAIN: &str = "the node is synced now; asking it again before halting:";

/// The start of the halt reason of a chain authority that lacks a call the follower needs.
pub const LACKS_CALL: &str = "the chain authority lacks a call the indexer needs";

/// The start of the gap reason of a lane proof the node can no longer serve.
pub const PROOF_TOO_DEEP: &str = "the node can no longer prove the chain at the cursor";

/// The text of a gap reason that says the node pruned the history the cursor stood in.
///
/// That is the case for a store that began at the pruning point of the node, and for a downtime
/// that exceeded its retention. A recovering follower ([`super::recovery`]) moves the store to the
/// pruning point of the node and follows from there with a new follower.
pub const PRUNED_GAP: &str = "downtime exceeded";

/// Whether a gap reason says the node pruned the history the cursor stood in, see [`PRUNED_GAP`].
pub fn is_pruned_gap(reason: &str) -> bool {
    reason.contains(PRUNED_GAP)
}

use super::ingest::{Cursor, Ingest, IngestError};
use super::prefetch::{Head, Prefetch, PrefetchLimits};
use super::status::{AuthorityPace, BackfillPace, CatchUp, FollowerState, HealthState, IndexEvent, PaceState};
use crate::config::StartMode;
use crate::hex::Hash32;
use crate::rpc::proof::{ChainProof, ChainProofConfig, ProofError};
use crate::rpc::types::{RawVspcResponse, VspcBatch, VspcRequest};
use crate::rpc::window::{BatchWindow, WindowPlan};
use crate::rpc::{ChainSource, NodeErrorKind, Origin, RpcError, WindowRequest};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, watch};

/// How often a halted follower repeats that it is halted, as an error line.
pub const HALT_REMIND: Duration = Duration::from_secs(600);

/// Chain requests of the authority that fail on the wire in a row (a timeout, a lost connection, an
/// undecodable answer) ...
pub const AUTHORITY_FAILURES: u32 = 2;
/// ... while the cursor does not move for this long, before the source is told that the authority
/// is failing (module documentation).
pub const AUTHORITY_STALL: Duration = Duration::from_secs(180);

/// The chain authority keeps up while the cursor block is at most this old by the local clock
/// (module documentation, *Progress*). The web leaves an indexer whose lag is above 120 s, so no
/// node may hold the index above that for good; an honest node at the tip is seconds behind.
pub const AUTHORITY_LAG_TOLERANCE: Duration = Duration::from_secs(90);

/// The follower's running time advances by at most this between two of its ticks (one a
/// second while a step or the pause after it runs). A longer gap is a pause of the follower
/// itself, which starts the progress rule's period again (module documentation, *Progress*).
pub const PAUSE_CAP: Duration = Duration::from_secs(30);

/// An authority that has kept up may spend at most this much of the follower's running time with
/// the lag above the tolerance within any [`EXCURSION_WINDOW`] of it; past it, it is failing
/// (module documentation, *Progress*). A lag that swings above the tolerance and back within each
/// period passes the period rule, and would keep the web leaving and coming back for good.
pub const EXCURSION_BUDGET: Duration = Duration::from_secs(300);
/// The rolling window of [`EXCURSION_BUDGET`], on the running time.
pub const EXCURSION_WINDOW: Duration = Duration::from_secs(3_600);

/// How often the running time ticks while a step or the pause after it runs ([`PAUSE_CAP`]).
const TICK: Duration = Duration::from_secs(1);

/// A cursor block stamped further ahead of the local clock than this says the host clock is slow:
/// consensus lets a miner stamp a block at most 132 s ahead of the network's time, plus 30 s.
pub const CLOCK_AHEAD_WARN: Duration = Duration::from_secs(132 + 30);

/// How often that is warned about at most, after the first time.
const CLOCK_WARN_EVERY: Duration = Duration::from_secs(3_600);

/// The header time of a cursor whose header was not read is asked again after this.
const TIME_ASK_AGAIN: Duration = Duration::from_secs(30);

/// Settings of a [`Follower`].
#[derive(Debug, Clone)]
pub struct FollowerConfig {
    /// The network the node must be on.
    pub network: String,
    /// Where a store with no cursor starts.
    pub start: StartMode,
    /// The pause between polls once the cursor is at the sink.
    pub poll_interval: Duration,
    /// The first retry delay. It doubles with each consecutive failure.
    pub backoff: Duration,
    /// The longest retry delay.
    pub max_backoff: Duration,
    /// Passed to the node as `minConfirmationCount`. `None` follows the tip, which is the default:
    /// the API reports confirmations per event and lets consumers choose the tier.
    pub min_confirmations: Option<u64>,
    /// How many stored blocks the walk-back considers when the node does not know the cursor.
    pub max_walkback_blocks: u64,
    /// Refreshes the node status (`getBlockDagInfo`) at most this often.
    pub status_refresh: Duration,
    /// The lag below which the follower reports `following` rather than `catching_up`.
    pub caught_up_daa: u64,
    /// The first batch window in blue score, about one block each at 10 BPS. It adapts from there.
    pub batch_initial_blue: u64,
    /// A batch should take about this long to fetch. The window shrinks above it and grows below
    /// half of it.
    pub batch_target: Duration,
    /// VSPC windows fetched at once while catching up. 1 fetches one at a time. The source needs
    /// that many connections.
    pub fetch_parallel: usize,
    /// Bytes of windows held ahead of the cursor, fetched or estimated in flight.
    pub prefetch_max_bytes: u64,
    /// Parallel fetch runs only while the cursor is more than this far behind the sink, in blue
    /// score.
    pub prefetch_min_lag_blue: u64,
    /// The first parallel window, in chain blocks. It adapts from there.
    pub prefetch_initial_blocks: usize,
    /// Chain requests of the authority that fail on the wire in a row ... ([`AUTHORITY_FAILURES`])
    pub authority_failures: u32,
    /// ... while the cursor does not move for this long, before another node may take over as the
    /// chain authority ([`AUTHORITY_STALL`], module documentation). It is also the period of the
    /// progress rule: above the tolerance, the lag must fall by half of it within it (module
    /// documentation, *Progress*).
    pub authority_stall: Duration,
    /// The lag of the cursor block behind the local clock up to which the authority keeps up
    /// ([`AUTHORITY_LAG_TOLERANCE`], module documentation, *Progress*).
    pub authority_lag_tolerance: Duration,
    /// The chain proofs of every batch (`rpc::proof`). `None`: off. They also need a source that
    /// serves them (`ChainSource::proves_chain`).
    pub chain_proof: Option<ChainProofConfig>,
}

impl FollowerConfig {
    /// Builds the follower settings from the indexer configuration.
    pub fn from_indexer(c: &crate::config::IndexerConfig) -> Self {
        FollowerConfig {
            network: c.network.clone(),
            start: c.start.clone(),
            poll_interval: Duration::from_millis(c.poll_interval_ms),
            backoff: Duration::from_millis(c.backoff_ms),
            max_backoff: Duration::from_millis(c.max_backoff_ms),
            min_confirmations: None,
            max_walkback_blocks: c.max_walkback_blocks,
            status_refresh: Duration::from_secs(2),
            caught_up_daa: 100,
            batch_initial_blue: 600,
            batch_target: (Duration::from_secs(c.rpc_timeout_secs) / 6).max(Duration::from_secs(1)),
            fetch_parallel: c.fetch_windows(),
            prefetch_max_bytes: c.prefetch_max_mb.saturating_mul(1 << 20),
            prefetch_min_lag_blue: c.prefetch_min_lag_blue,
            prefetch_initial_blocks: 64,
            authority_failures: AUTHORITY_FAILURES,
            authority_stall: AUTHORITY_STALL,
            authority_lag_tolerance: AUTHORITY_LAG_TOLERANCE,
            chain_proof: Some(c.chain_proof.clone()).filter(|p| p.enabled),
        }
    }

    fn window(&self) -> BatchWindow {
        BatchWindow::new(self.batch_initial_blue, 1, 1 << 20, self.batch_target)
    }

    fn prefetch_limits(&self) -> PrefetchLimits {
        PrefetchLimits {
            parallel: self.fetch_parallel.max(1),
            max_bytes: self.prefetch_max_bytes,
            // Windows in flight share the link, so each may take longer than a lone batch: up to
            // half the request timeout.
            target: self.batch_target.saturating_mul(self.fetch_parallel.clamp(1, 3) as u32),
            initial_blocks: self.prefetch_initial_blocks,
            min_confirmations: self.min_confirmations,
        }
    }
}

/// What one step of the follower did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// A batch was committed. `more` is true when the node likely has more chain to give at once.
    Applied { added: usize, removed: usize, more: bool },
    /// The cursor is at the node's sink.
    Idle,
    /// Cursor rewound to an older stored block the node still knows.
    Rewound { blocks: usize },
    /// A bounded request came back without a chain block, because the next one lies beyond the
    /// window. The window grew, so ask again at once.
    Widened { window: u64 },
    /// A transient failure. Back off.
    Retry(String),
    /// An unrecoverable failure. It needs operator action.
    Gap(String),
    /// Writing the record log failed, for example on a full disk or an I/O error. The follower
    /// stops and appends nothing more until the operator has fixed the cause and restarted. The
    /// `open` of the restart checks the log.
    Halted(String),
    /// The authority's chain requests kept failing and another node is the chain authority now
    /// (module documentation). Ask it at once.
    Handover(String),
}

/// How a batch about to be committed was fetched.
struct Committed {
    /// The chain block it was requested from, which must be the cursor.
    start: Hash32,
    /// The window, not the sink, ended it, so more chain follows.
    by_window: bool,
    t_fetch: Duration,
    t_parse: Duration,
    /// The chain proofs, and for a window from another node its comparison with the primary.
    t_check: Duration,
    /// The blue score of its last chain block, when known. Prefetched windows learn it.
    end_blue: Option<u64>,
    /// It is one of the windows fetched ahead. The sequential window learns nothing from it.
    prefetched: bool,
}

/// Follows the VSPC v2 chain of a node into the store.
pub struct Follower<S: ChainSource> {
    source: Arc<S>,
    ingest: Arc<Mutex<Ingest>>,
    cfg: FollowerConfig,
    health: Arc<HealthState>,
    events: broadcast::Sender<Arc<IndexEvent>>,
    last_status: Mutex<Option<std::time::Instant>>,
    window: Mutex<BatchWindow>,
    /// The blue score of the cursor block, once known from a header or `getBlock`.
    cursor_blue: Mutex<Option<(Hash32, u64)>>,
    /// The timestamp (Unix ms) of the cursor block, once a batch applied it.
    cursor_ts: Mutex<Option<(Hash32, u64)>>,
    /// The windows fetched ahead of the cursor (parallel fetch).
    prefetch: tokio::sync::Mutex<Prefetch>,
    /// When the next pruning batch of lean storage or archive pruning is due. `None`: no pruning.
    lean_next: Mutex<Option<std::time::Instant>>,
    /// The catch-up pace, from the lag over the last minute, and when its seconds count from.
    pace: Mutex<(BackfillPace, std::time::Instant)>,
    /// When the free space of the data directory and the size of the database are measured next,
    /// see [`Follower::disk_tick`].
    disk_next: Mutex<std::time::Instant>,
    /// The wall-clock minute (ms) the books were last sampled, see [`Follower::quotes_tick`].
    quotes_minute: Mutex<i64>,
    /// The authority's chain requests that failed on the wire in a row, and the last of them.
    chain_failures: Mutex<(u32, String)>,
    /// When the cursor last moved, or the authority last answered from it (the local monotonic
    /// clock).
    progress_at: Mutex<std::time::Instant>,
    /// Tip reorgs since the cursor last moved ([`TIP_REORG_STALL`]).
    tip_reorgs: Mutex<TipReorgs>,
    /// The chain proofs (`rpc::proof`), when on and the source serves them.
    proof: Option<Arc<Mutex<ChainProof>>>,
    /// A contradiction of the store was put off because the node was not synced.
    contradiction_unsynced: std::sync::atomic::AtomicBool,
    /// The `ChainSource::authority_key` whose calls passed the check.
    checked_authority: Mutex<Option<String>>,
    /// The local monotonic clock as Unix ms: an instant and the wall clock then.
    clock: (tokio::time::Instant, u64),
    /// The cursor whose header time was last asked of the node, and when.
    time_asked: Mutex<Option<(Hash32, tokio::time::Instant)>>,
    /// The progress rule (module documentation, *Progress*).
    rule: Mutex<Rule>,
    /// The follower's own database tasks running now: their time is not the authority's.
    own_busy: std::sync::atomic::AtomicUsize,
    /// The cursor the last step stood at.
    cursor_seen: Mutex<Option<Hash32>>,
}

/// The state of the progress rule (module documentation, *Progress*).
#[derive(Debug)]
struct Rule {
    /// The follower's running time in ms, and its last tick.
    run_ms: u64,
    last_tick: tokio::time::Instant,
    /// The authority (`ChainSource::authority_node`) the period belongs to.
    authority: Option<String>,
    /// The period while the lag is above the tolerance, or was within the last period.
    period: Option<Period>,
    /// The last period ended without the gain, and the lag has not come within the tolerance
    /// since.
    failing: bool,
    /// The follower paused: the period starts again at the next check.
    restart: bool,
    /// A period that failed, to hand the authority over for: why.
    failed: Option<String>,
    /// When a slow host clock was last warned about.
    clock_warned: Option<tokio::time::Instant>,
    /// This authority has kept up (its lag within the tolerance) since it leads: from then on its
    /// time above the tolerance is charged to [`EXCURSION_BUDGET`].
    kept_up: bool,
    /// The running time of the last check and whether the lag was above the tolerance then.
    last_check: Option<(u64, bool)>,
    /// The spans of running time this authority spent above the tolerance after it kept up, oldest
    /// first, within the last [`EXCURSION_WINDOW`].
    excursions: std::collections::VecDeque<(u64, u64)>,
    /// The authority deposed last, until its successor shows whether it was to blame.
    blame: Option<Blame>,
}

/// A deposal waiting for its successor ([`ChainSource::deposal_confirmed`]).
#[derive(Debug, Clone)]
struct Blame {
    /// The deposed authority (`ChainSource::authority_node`).
    deposed: String,
    /// The cursor block's time at the deposal, and the local clock then (ms).
    cursor_ts: Option<u64>,
    at_ms: u64,
    /// The successor applied a block stamped between the two: chain that existed and the deposed
    /// authority did not serve.
    withheld: bool,
}

/// A period of the progress rule.
#[derive(Debug, Clone, Copy)]
struct Period {
    /// Its start on the running time, and the lag then (`None`: the time was not known; the first
    /// known lag takes its place).
    start_ms: u64,
    start_lag: Option<i64>,
    /// Since when the lag is within the tolerance (running time).
    calm_since: Option<u64>,
}

/// How often the free space of the data directory and the size of the database are measured.
const DISK_EVERY: Duration = Duration::from_secs(60);

/// Lean storage: the pause between batches while one hit its limits and there is more to delete.
const LEAN_BUSY_EVERY: Duration = Duration::from_millis(100);
/// Lean storage: the pause after a batch that found nothing more to delete.
const LEAN_IDLE_EVERY: Duration = Duration::from_secs(30);
/// Lean storage prunes only while the store follows the node. When the store is behind the node by
/// more than this many DAA, a minute at 10 blocks a second, it catches up first with every step of
/// the follower going to the chain, and the pruning waits.
const LEAN_MAX_LAG_DAA: u64 = 600;

/// Chain-proof requests of one batch in flight at once: the merged blocks of its transactions that
/// can matter to KOB, and the lanes new at the anchor.
///
/// Each is a round trip to the primary, about 0.3 s to a remote node. Asked one after another, the
/// merged blocks alone cost a window of the TN10 soak (32 chain blocks, 4 s of chain, 5 or more
/// such blocks) 2 s or more, after its parallel fetch: the follower ran at 1 to 1.5 times chain
/// speed whatever `fetch_parallel` was, and fell behind after every restart.
///
/// The node clients of the indexer have as many connections for the small calls
/// (`rpc::WrpcConfig::small_connections`), since a connection carries one request at a time.
pub const PROOF_FETCHES: usize = 8;

/// A merged block of a batch: its hash, the chain blocks that accepted its transactions that can
/// matter to KOB, those transactions, and the block.
type MergedBlock<'a> = (Hash32, Vec<usize>, Vec<&'a crate::rpc::types::Tx>, (kaspa_consensus_core::block::Block, Origin));

/// Runs of flagged chain blocks of a window from `start` (chain `hashes`), as (the chain block
/// before the run, the index of its first block, the number of blocks). Runs with no more than
/// `MERGE_GAP` unflagged chain blocks between them are fetched as one, because a round trip costs
/// more than a few chain blocks.
fn flag_runs(flags: &[bool], start: Hash32, hashes: &[Hash32]) -> Vec<(Hash32, usize, usize)> {
    const MERGE_GAP: usize = 3;
    let mut runs: Vec<(Hash32, usize, usize)> = vec![];
    for (i, f) in flags.iter().enumerate() {
        if !*f {
            continue;
        }
        match runs.last_mut() {
            Some((_, first, len)) if *first + *len + MERGE_GAP >= i => *len = i + 1 - *first,
            _ => runs.push((if i == 0 { start } else { hashes[i - 1] }, i, 1)),
        }
    }
    // More runs than that are one span from the first flagged block to the last: the cross-check
    // then asks at most MAX_BODY_RUNS requests per window, or one copy of the span (R2C-L2).
    if runs.len() > MAX_BODY_RUNS {
        let (before, first, _) = runs[0];
        let (last, len) = runs.last().map(|r| (r.1, r.2)).expect("runs");
        runs = vec![(before, first, last + len - first)];
    }
    runs
}

/// Most runs of flagged chain blocks a window is re-read in ([`flag_runs`]).
pub const MAX_BODY_RUNS: usize = 4;

/// `ask` of each of `items`, at most `limit` in flight, the answers in the order of `items`.
///
/// Nothing is asked before the stream is polled. Dropping it cancels what is in flight, so a
/// caller that stops at a failure asks for nothing more.
fn asked_in_order<T, F, Fut>(items: impl IntoIterator<Item = T>, limit: usize, ask: F) -> impl futures_util::Stream<Item = Fut::Output>
where
    F: FnMut(T) -> Fut,
    Fut: std::future::Future,
{
    futures_util::stream::iter(items).map(ask).buffered(limit.max(1))
}

/// Whether a step failure is routine and not worth a warning.
///
/// A routine failure is the tips of two nodes differing for a moment, until it has repeated as
/// often as it takes to replace a node (`attempt` failures in a row), and a tip reorg
/// ([`TIP_REORG`]) that did not repeat yet.
fn retry_is_routine(reason: &str, attempt: u32) -> bool {
    // Waiting for a cross-checking node is logged by `rpc::multi` and shown in `/v1/health`.
    reason.contains(crate::rpc::multi::WAITING_FOR_CROSS_CHECK)
        || (reason.contains(crate::rpc::multi::TIPS_DIFFER) && attempt < crate::rpc::multi::STALLS_BEFORE_REPLACING)
        || (reason.starts_with(TIP_REORG) && attempt < 3)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl<S: ChainSource> Follower<S> {
    /// Creates a follower of `source` that commits to `ingest` and reports to `health`.
    pub fn new(
        source: Arc<S>,
        ingest: Arc<Mutex<Ingest>>,
        cfg: FollowerConfig,
        health: Arc<HealthState>,
        events: broadcast::Sender<Arc<IndexEvent>>,
    ) -> Self {
        let window = Mutex::new(cfg.window());
        let prefetch = tokio::sync::Mutex::new(Prefetch::new(cfg.prefetch_limits()));
        let lean = ingest.lock().unwrap_or_else(|e| e.into_inner()).prunes();
        let clock_at = source.simulated_clock_ms().unwrap_or_else(now_ms);
        let proof =
            cfg.chain_proof.clone().filter(|_| source.proves_chain()).map(|p| Arc::new(Mutex::new(ChainProof::new(p, &cfg.network))));
        if let Some(p) = &proof {
            // The floor's targets of the last run (K-M3), not the fixed minimum.
            let g = ingest.lock().unwrap_or_else(|e| e.into_inner());
            if let (Ok(Some(s)), Ok(Some(c))) = (super::db::meta_get(g.conn(), crate::rpc::proof::FLOOR_META_KEY), g.cursor()) {
                let n = p.lock().unwrap_or_else(|e| e.into_inner()).seed_floor(&s, c.daa);
                tracing::info!(headers = n, "chain proofs: the difficulty floor starts from the stored targets");
            }
        }
        Follower {
            source,
            ingest,
            cfg,
            health,
            events,
            last_status: Mutex::new(None),
            window,
            cursor_blue: Mutex::new(None),
            cursor_ts: Mutex::new(None),
            prefetch,
            lean_next: Mutex::new(lean.then(std::time::Instant::now)),
            pace: Mutex::new((BackfillPace::default(), std::time::Instant::now())),
            disk_next: Mutex::new(std::time::Instant::now()),
            quotes_minute: Mutex::new(0),
            chain_failures: Mutex::new((0, String::new())),
            progress_at: Mutex::new(std::time::Instant::now()),
            tip_reorgs: Mutex::new(TipReorgs::default()),
            proof,
            contradiction_unsynced: std::sync::atomic::AtomicBool::new(false),
            checked_authority: Mutex::new(None),
            clock: (tokio::time::Instant::now(), clock_at),
            time_asked: Mutex::new(None),
            rule: Mutex::new(Rule {
                run_ms: 0,
                last_tick: tokio::time::Instant::now(),
                authority: None,
                period: None,
                failing: false,
                restart: false,
                failed: None,
                clock_warned: None,
                kept_up: false,
                last_check: None,
                excursions: Default::default(),
                blame: None,
            }),
            own_busy: std::sync::atomic::AtomicUsize::new(0),
            cursor_seen: Mutex::new(None),
        }
    }

    /// Sets the local clock to read `unix_ms` now (tests whose blocks carry a fixed time). It runs
    /// on from there with the monotonic clock.
    pub fn set_clock_at(&mut self, unix_ms: u64) {
        self.clock = (tokio::time::Instant::now(), unix_ms);
    }

    /// The local monotonic clock in Unix ms: the wall clock at the start plus the monotonic time
    /// since, so a step of the wall clock does not move it.
    fn mono_ms(&self) -> u64 {
        self.clock.1 + self.clock.0.elapsed().as_millis() as u64
    }

    fn rule(&self) -> std::sync::MutexGuard<'_, Rule> {
        self.rule.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One tick of the follower's running time (module documentation, *Progress*): the time
    /// since the last tick counts when `counted` and no database task of the follower runs; a gap
    /// longer than [`PAUSE_CAP`] is a pause of the follower, which starts the period again.
    fn tick(&self, counted: bool) {
        let now = tokio::time::Instant::now();
        let mut r = self.rule();
        let gap = now.saturating_duration_since(r.last_tick);
        r.last_tick = now;
        if gap > PAUSE_CAP {
            r.restart = true;
        } else if counted && self.own_busy.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            r.run_ms = r.run_ms.saturating_add(gap.as_millis() as u64);
        }
    }

    /// `f`, with the running time ticking while it runs.
    async fn ticked<T>(&self, f: impl std::future::Future<Output = T>) -> T {
        let mut f = std::pin::pin!(f);
        loop {
            tokio::select! {
                biased;
                o = &mut f => return o,
                _ = tokio::time::sleep(TICK) => self.tick(true),
            }
        }
    }

    /// The progress rule at the cursor `cursor` (module documentation, *Progress*): the verdict,
    /// also published in `/v1/health` (`authority_pace`) and told the source
    /// ([`ChainSource::deposal_confirmed`]). A period that failed is left for [`Follower::hand_over`].
    async fn pace(&self, cursor: Hash32) -> PaceState {
        self.learn_cursor_time(cursor).await;
        let ts = self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()).filter(|(h, _)| *h == cursor).map(|(_, t)| t);
        let lag = ts.map(|t| self.mono_ms() as i64 - t as i64);
        let key = self.source.authority_node();
        let tol = self.cfg.authority_lag_tolerance.as_millis() as i64;
        let stall = self.cfg.authority_stall.as_millis() as u64;
        let budget = EXCURSION_BUDGET.as_millis() as u64;
        let window = EXCURSION_WINDOW.as_millis() as u64;
        let (state, warn_clock, confirm) = {
            let mut g = self.rule();
            let r = &mut *g;
            if r.authority.as_deref() != Some(key.as_str()) {
                // A new authority, or the first step: it begins with no period, and has not kept up.
                r.authority = Some(key.clone());
                r.period = None;
                r.failing = false;
                r.failed = None;
                r.kept_up = false;
                r.last_check = None;
                r.excursions.clear();
            }
            let run = r.run_ms;
            if std::mem::take(&mut r.restart) {
                if let Some(p) = r.period.as_mut() {
                    *p = Period { start_ms: run, start_lag: lag, calm_since: None };
                }
            }
            // The running time since the last check is charged as the lag stood then: above the
            // tolerance, after this authority kept up, it is an excursion. The follower's own
            // pauses are not on the running time, so they charge nothing.
            let above = lag.is_none_or(|l| l > tol);
            if let Some((prev, was_above)) = r.last_check {
                if was_above && r.kept_up && run > prev {
                    match r.excursions.back_mut() {
                        Some(last) if last.1 == prev => last.1 = run,
                        _ => r.excursions.push_back((prev, run)),
                    }
                }
            }
            r.last_check = Some((run, above));
            let from = run.saturating_sub(window);
            while r.excursions.front().is_some_and(|e| e.1 <= from) {
                r.excursions.pop_front();
            }
            let used: u64 = r.excursions.iter().map(|&(a, b)| b - a.max(from)).sum();
            let mut gained_period = false;
            let state = match lag {
                Some(l) if l <= tol => {
                    if let Some(p) = r.period.as_mut() {
                        let calm = *p.calm_since.get_or_insert(run);
                        if run.saturating_sub(calm) >= stall {
                            r.period = None;
                        }
                    }
                    r.failing = false;
                    r.kept_up = true;
                    PaceState::KeepingUp
                }
                _ => {
                    let p = r.period.get_or_insert(Period { start_ms: run, start_lag: lag, calm_since: None });
                    p.calm_since = None;
                    if p.start_lag.is_none() {
                        p.start_lag = lag;
                    }
                    if run.saturating_sub(p.start_ms) >= stall {
                        let need = (stall / 2) as i64;
                        let gained = matches!((p.start_lag, lag), (Some(a), Some(b)) if a - b >= need);
                        if !gained {
                            let secs = |l: Option<i64>| l.map_or("an unknown time".to_string(), |l| format!("{} s", l / 1_000));
                            r.failed = Some(format!(
                                "the chain authority does not keep up with real time: in {} s of the follower's running time the cursor's chain block went from {} to {} behind the local clock; it must gain {} s on it, or come within {} s",
                                stall / 1_000,
                                secs(p.start_lag),
                                secs(lag),
                                need / 1_000,
                                tol / 1_000
                            ));
                        }
                        gained_period = gained;
                        r.failing = !gained;
                        *p = Period { start_ms: run, start_lag: lag, calm_since: None };
                    }
                    // Past the budget of excursions: failing as well, and the budget starts again.
                    if !r.failing && r.kept_up && used > budget {
                        r.failed = Some(format!(
                            "the chain authority does not keep up with real time: after it kept up it held the lag above {} s for {} s of the follower's running time within the last {} minutes (at most {} s)",
                            tol / 1_000,
                            used / 1_000,
                            window / 60_000,
                            budget / 1_000
                        ));
                        r.failing = true;
                        r.excursions.clear();
                    }
                    match (r.failing, lag) {
                        (true, _) => PaceState::Failing,
                        (false, None) => PaceState::Unknown,
                        (false, Some(_)) => PaceState::Gaining,
                    }
                }
            };
            // The authority deposed last: to blame once its successor keeps up after serving chain
            // the deposed one had not; no one is, if the successor fails too, or the deposed one
            // leads again.
            let mut confirm = false;
            if let Some(b) = &r.blame {
                if b.deposed == key || state == PaceState::Failing {
                    r.blame = None;
                } else if b.withheld && (state == PaceState::KeepingUp || gained_period) {
                    confirm = true;
                    r.blame = None;
                } else if ts.is_some_and(|t| t >= b.at_ms) {
                    // It served everything that existed at the deposal, none of it held back.
                    r.blame = None;
                }
            }
            // The cursor block stamped far ahead of the local clock: the host clock is slow.
            let ahead = lag.is_some_and(|l| -l > CLOCK_AHEAD_WARN.as_millis() as i64);
            let warn = ahead && r.clock_warned.is_none_or(|t| t.elapsed() >= CLOCK_WARN_EVERY);
            if warn {
                r.clock_warned = Some(tokio::time::Instant::now());
            }
            (state, warn, confirm)
        };
        if warn_clock {
            tracing::warn!(
                ahead_secs = lag.map_or(0, |l| -l / 1_000),
                "the cursor's chain block is stamped ahead of the local clock by more than a miner may stamp it: the host clock is slow (set it: NTP). The rule that judges the chain authority assumes a clock within 30 s of real time"
            );
        }
        if confirm {
            tracing::info!("the chain authority deposed last held back chain its successor served: it stays away longer (a strike)");
            self.source.deposal_confirmed();
        }
        let pace = AuthorityPace {
            state,
            lag_ms: lag,
            tolerance_ms: self.cfg.authority_lag_tolerance.as_millis() as u64,
            at_unix_ms: now_ms(),
        };
        self.health.update(|h| {
            h.authority_pace = Some(pace);
            if matches!(state, PaceState::KeepingUp | PaceState::Gaining) {
                h.last_progress_unix_ms = now_ms();
            }
        });
        state
    }

    /// The header time of the cursor's chain block, from its header when no batch applied it
    /// (after a start, a rewind, or a batch whose header did not hash with chain proofs off),
    /// asked at most every [`TIME_ASK_AGAIN`]. A header counts only when it hashes to the cursor.
    async fn learn_cursor_time(&self, cursor: Hash32) {
        if self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|(h, _)| h == cursor) {
            return;
        }
        {
            let mut asked = self.time_asked.lock().unwrap_or_else(|e| e.into_inner());
            if asked.is_some_and(|(h, t)| h == cursor && t.elapsed() < TIME_ASK_AGAIN) {
                return;
            }
            *asked = Some((cursor, tokio::time::Instant::now()));
        }
        if let Ok(Some((b, _))) = self.source.block(cursor, false).await {
            let h = &b.header;
            if h.hash.as_bytes() == cursor.0 && kaspa_consensus_core::hashing::header::hash(h) == h.hash {
                *self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()) = Some((cursor, h.timestamp));
                self.health.update(|s| s.cursor_time_unix_ms = Some(h.timestamp));
            }
        }
    }

    /// A chain request of the authority (`what`) failed: counted when it failed on the wire.
    fn chain_failed(&self, what: &str, e: &RpcError) {
        if crate::rpc::failover::node_failure(e) {
            self.count_chain_failure(format!("{what}: {e}"));
        }
    }

    fn count_chain_failure(&self, what: String) {
        let mut g = self.chain_failures.lock().unwrap_or_else(|e| e.into_inner());
        g.0 = g.0.saturating_add(1);
        g.1 = what;
    }

    /// The authority answered from the cursor, or the cursor moved: the failures on the wire in a
    /// row and their stall begin again. Whether an answer is progress is the progress rule's
    /// ([`Follower::pace`]).
    fn chain_answered(&self) {
        self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
        *self.progress_at.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
        *self.tip_reorgs.lock().unwrap_or_else(|e| e.into_inner()) = TipReorgs::default();
    }

    /// After a step: when a period of the progress rule failed (module documentation,
    /// *Progress*), or the authority's chain requests failed on the wire often enough in a row
    /// while the cursor stalled long enough, the source is told
    /// ([`ChainSource::authority_failing`]). `Some`: another node is the authority now.
    async fn hand_over(&self) -> Option<StepOutcome> {
        let behind = self.rule().failed.take();
        let (n, last) = self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let stalled = self.progress_at.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
        let reorg_stall = std::mem::take(&mut self.tip_reorgs.lock().unwrap_or_else(|e| e.into_inner()).stalled);
        let wire = n >= self.cfg.authority_failures.max(1) && (stalled >= self.cfg.authority_stall || reorg_stall);
        if behind.is_none() && !wire {
            return None;
        }
        let cursor = self.blocking(|i| i.cursor()).await.ok().flatten()?;
        let why = match behind {
            Some(why) => why,
            None => format!("{n} chain requests failed in a row, the last {last}; no progress for {} s", stalled.as_secs()),
        };
        let moved = self.source.authority_failing(cursor.hash, &why).await;
        // Whatever the source did, the count begins again: the next try needs as many failures.
        // With no other node, the next period of the progress rule asks again.
        self.chain_failures.lock().unwrap_or_else(|e| e.into_inner()).0 = 0;
        if !moved {
            return None;
        }
        // The new authority gets a period of its own. The deposed one is to blame (a strike, longer
        // away) only if its successor keeps up after serving chain that existed at the deposal.
        let cursor_ts = self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()).filter(|(h, _)| *h == cursor.hash).map(|(_, t)| t);
        {
            let mut r = self.rule();
            let deposed = r.authority.take().unwrap_or_default();
            r.blame = Some(Blame { deposed, cursor_ts, at_ms: self.mono_ms(), withheld: false });
            r.period = None;
            r.failing = false;
        }
        *self.progress_at.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
        // The plan was made on the old authority's chain, and the window sized for its link.
        self.prefetch.lock().await.discard();
        *self.win() = self.cfg.window();
        let msg = format!("the chain authority's chain requests kept failing ({why}): another node is the chain authority now");
        self.set_state(FollowerState::CatchingUp, Some(msg.clone()));
        Some(StepOutcome::Handover(msg))
    }

    /// Measures about once a minute the free bytes of the file system of the data directory and
    /// the size of the database.
    ///
    /// They are published in `/v1/health`, `kob_data_dir_free_bytes` and `kob_indexer_db_bytes`, so
    /// a disk that fills up is seen before it stops the store.
    async fn disk_tick(&self) {
        {
            let mut next = self.disk_next.lock().unwrap_or_else(|e| e.into_inner());
            if *next > std::time::Instant::now() {
                return;
            }
            *next = std::time::Instant::now() + DISK_EVERY;
        }
        let db = self.blocking(|i| i.conn().path().filter(|p| !p.is_empty()).map(std::path::PathBuf::from)).await;
        let Some(db) = db else { return };
        let (free, size) = tokio::task::spawn_blocking(move || {
            let dir = db.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
            (crate::disk::free_bytes(&dir), crate::disk::db_bytes(&db))
        })
        .await
        .unwrap_or((None, None));
        self.health.update(|h| {
            h.data_dir_free_bytes = free;
            h.db_bytes = size;
        });
    }

    /// Samples the top of the KAS books once a wall-clock minute ([`super::quotes`]).
    ///
    /// A store behind the node has an old book, so it samples nothing.
    async fn quotes_tick(&self) {
        let now = now_ms() as i64;
        let minute = now - now.rem_euclid(60_000);
        {
            let mut m = self.quotes_minute.lock().unwrap_or_else(|e| e.into_inner());
            if *m >= minute {
                return;
            }
            *m = minute;
        }
        let snap = self.health.snapshot();
        if !snap.within_lag_tolerance() {
            return;
        }
        // The settle depth plays no part in a book.
        let ctx = super::reads::ReadCtx { node_daa: snap.node_daa, settle_depth_daa: 0, now_unix: Some((now / 1_000) as u64) };
        if let Err(e) = self.blocking(move |i| i.sample_quotes(&ctx, now)).await {
            tracing::warn!(error = %e, "quotes: sampling the books failed; trying again next minute");
        }
    }

    /// Updates how fast the store closes in on the node and when it will have, while it is
    /// catching up.
    ///
    /// The rate comes from the lag over the last minute and is published as `catch_up` of the
    /// health and the status line. It is cleared once the store follows.
    fn note_catch_up(&self) {
        let h = self.health.snapshot();
        let lag = h.lag_daa().filter(|_| h.state == FollowerState::CatchingUp && h.cursor_daa > 0);
        let catch_up = lag.and_then(|l| {
            let mut g = self.pace.lock().unwrap_or_else(|e| e.into_inner());
            let at = g.1.elapsed().as_secs_f64();
            let (rate, eta) = g.0.push(at, l, h.cursor_daa);
            rate.map(|rate_daa_per_sec| CatchUp { rate_daa_per_sec, eta_seconds: eta })
        });
        if lag.is_none() {
            self.pace.lock().unwrap_or_else(|e| e.into_inner()).0 = BackfillPace::default();
        }
        if catch_up != h.catch_up {
            self.health.update(|h| h.catch_up = catch_up);
        }
    }

    /// Runs one bounded pruning batch of lean storage when it is due, see [`super::lean`].
    ///
    /// The follower runs it between its steps, so the store has one writer and the batch, a short
    /// transaction, never overlaps a commit.
    async fn lean_tick(&self) {
        let due = self.lean_next.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t <= std::time::Instant::now());
        if !due {
            return;
        }
        let snap = self.health.snapshot();
        if snap.lag_daa().is_some_and(|lag| lag > LEAN_MAX_LAG_DAA) {
            *self.lean_next.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now() + LEAN_BUSY_EVERY);
            return;
        }
        let node_daa = snap.node_daa;
        let began = std::time::Instant::now();
        let next = match self.blocking(move |i| i.prune_batch(node_daa)).await {
            Ok(Some(rep)) => {
                if rep.over_cap != snap.lean_over_cap {
                    if rep.over_cap {
                        tracing::warn!("lean storage: the store is above its size cap (lean_max_db_mb); pruning closed orders to the reorg window");
                    }
                    self.health.update(|h| h.lean_over_cap = rep.over_cap);
                }
                if rep.rows > 0 || rep.pages > 0 {
                    tracing::debug!(orders = rep.orders, rows = rep.rows, pages = rep.pages, more = rep.more, "storage: pruned");
                    self.health.update(|h| {
                        h.lean_orders_pruned_total += rep.orders as u64;
                        h.lean_rows_pruned_total += rep.rows as u64;
                    });
                }
                if rep.more {
                    // A pause of twice the batch, so pruning takes a third of the thread at most,
                    // and at least the base pause.
                    LEAN_BUSY_EVERY.max(began.elapsed() * 2)
                } else {
                    LEAN_IDLE_EVERY
                }
            }
            Ok(None) => LEAN_IDLE_EVERY,
            Err(e) => {
                tracing::warn!(error = %e, "storage: pruning failed; trying again later");
                self.health.update(|h| h.lean_prune_failures_total += 1);
                LEAN_IDLE_EVERY
            }
        };
        *self.lean_next.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now() + next);
    }

    fn win(&self) -> std::sync::MutexGuard<'_, BatchWindow> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The blue score of the cursor, remembered from the last applied header, else asked from the
    /// node.
    async fn cursor_blue(&self, cursor: Hash32) -> Result<Option<u64>, RpcError> {
        if let Some((h, b)) = *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) {
            if h == cursor {
                return Ok(Some(b));
            }
        }
        let b = self.source.block_blue_score(cursor).await?;
        if let Some(b) = b {
            *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) = Some((cursor, b));
        }
        Ok(b)
    }

    /// The window of the next request from `cursor`.
    ///
    /// A `None` blue score of the cursor means the node does not know it. The unbounded request
    /// then fails fast with `cannot find header` and the walk-back runs.
    async fn plan(&self, cursor: Hash32) -> Result<WindowPlan, RpcError> {
        let extra = self.cfg.min_confirmations;
        let Some(cursor_blue) = self.cursor_blue(cursor).await? else {
            return Ok(WindowPlan { min_confirmations: extra, until_blue: None, span: u64::MAX, timeout_scale: 1 });
        };
        let sink_blue = self.source.sink_blue_score().await?;
        let (plan, window) = {
            let w = self.win();
            (w.plan(cursor_blue, sink_blue, extra), w.current())
        };
        self.health.update(|h| {
            h.lag_blue = Some(sink_blue.saturating_sub(cursor_blue));
            h.batch_window_blue = Some(window);
        });
        Ok(plan)
    }

    /// `f` on the store, on the blocking pool. Its time is the follower's own, not the authority's:
    /// it does not count on the running time, and one longer than [`PAUSE_CAP`] (a slow disk) is a
    /// pause of the follower (module documentation, *Progress*).
    async fn blocking<T: Send + 'static>(&self, f: impl FnOnce(&mut Ingest) -> T + Send + 'static) -> T {
        use std::sync::atomic::Ordering::Relaxed;
        let ing = self.ingest.clone();
        self.tick(true);
        self.own_busy.fetch_add(1, Relaxed);
        let t0 = tokio::time::Instant::now();
        let r = tokio::task::spawn_blocking(move || f(&mut ing.lock().unwrap_or_else(|e| e.into_inner()))).await;
        self.own_busy.fetch_sub(1, Relaxed);
        if t0.elapsed() > PAUSE_CAP {
            self.rule().restart = true;
        }
        self.tick(false);
        r.expect("blocking task panicked")
    }

    fn set_state(&self, state: FollowerState, err: Option<String>) {
        self.health.update(|h| {
            h.state = state;
            h.last_poll_unix_ms = now_ms();
            // Progress ends a halt or a storage error. A node error between two failed commits does
            // not.
            if matches!(state, FollowerState::Following | FollowerState::CatchingUp) {
                h.halt_reason = None;
            }
            if let Some(e) = err {
                h.last_error = Some(e);
            }
        });
    }

    /// A record-log write failed. Stops following, with state `stopped` and the reason in
    /// `last_error` and `halt_reason`.
    fn halt(&self, e: &IngestError) -> StepOutcome {
        let msg = format!("record log write failed, the follower stopped (free space or fix the disk, then restart): {e}");
        self.set_state(FollowerState::Stopped, Some(msg.clone()));
        self.health.update(|h| h.halt_reason = Some(msg.clone()));
        StepOutcome::Halted(msg)
    }

    /// The database cannot be written or is corrupt (`what`, [`super::db::storage_problem`]).
    ///
    /// The state is `storage_error`, with the reason in `last_error` and `halt_reason`. The
    /// follower retries, so a disk with free space again goes on.
    fn storage_error(&self, what: &str, e: &IngestError) -> StepOutcome {
        let msg = format!("{what}: {e}");
        tracing::error!("the database cannot be written: {msg}");
        // The follower's own failure: its time is not the authority's (module documentation,
        // *Progress*).
        self.rule().restart = true;
        self.set_state(FollowerState::StorageError, Some(msg.clone()));
        self.health.update(|h| h.halt_reason = Some(msg.clone()));
        StepOutcome::Retry(msg)
    }

    async fn ensure_cursor(&self) -> Result<Cursor, StepOutcome> {
        if let Some(c) = self.blocking(|i| i.cursor()).await.map_err(|e| StepOutcome::Retry(format!("database: {e}")))? {
            *self.cursor_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(c.hash);
            return Ok(c);
        }
        let info = self.source.server_info().await.map_err(|e| StepOutcome::Retry(format!("getServerInfo: {e}")))?;
        if info.network_id != self.cfg.network {
            return Err(StepOutcome::Gap(format!("node is on `{}`, configured `{}`", info.network_id, self.cfg.network)));
        }
        if !info.is_synced {
            return Err(StepOutcome::Retry("node is not synced yet".into()));
        }
        self.health.update(|h| h.node_version = Some(info.server_version.clone()));
        let hash = match &self.cfg.start {
            StartMode::Hash(h) => *h,
            StartMode::PruningPoint => {
                self.source.dag_info().await.map_err(|e| StepOutcome::Retry(format!("getBlockDagInfo: {e}")))?.pruning_point_hash
            }
            StartMode::Sink => self.source.dag_info().await.map_err(|e| StepOutcome::Retry(format!("getBlockDagInfo: {e}")))?.sink,
        };
        // A start at the sink knows its position, so the book of a quiet chain is not taken for a
        // stale one. The other starts learn it from their first batch.
        let daa = match self.cfg.start {
            StartMode::Sink => self.source.block_daa_score(hash).await.ok().flatten().unwrap_or(0),
            _ => 0,
        };
        let c = Cursor { hash, daa };
        // A start anywhere but the pruning point leaves the older history out, so the book is not
        // complete, for good.
        let partial = !matches!(self.cfg.start, StartMode::PruningPoint);
        self.blocking(move |i| {
            i.init_cursor(&c)?;
            if partial {
                i.mark_history_from(c.daa)?;
            }
            Ok(())
        })
        .await
        .map_err(|e: IngestError| StepOutcome::Retry(format!("database: {e}")))?;
        if partial {
            self.health.update(|h| {
                h.book_complete = false;
                h.history_from_daa = Some(c.daa);
            });
        }
        tracing::info!(start = %hash, mode = ?self.cfg.start, "initialised cursor");
        *self.cursor_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(c.hash);
        Ok(c)
    }

    async fn refresh_status(&self) {
        let due = {
            let mut g = self.last_status.lock().unwrap_or_else(|e| e.into_inner());
            let due = g.map(|t| t.elapsed() >= self.cfg.status_refresh).unwrap_or(true);
            if due {
                *g = Some(std::time::Instant::now());
            }
            due
        };
        if !due {
            return;
        }
        match self.source.dag_info().await {
            Ok(d) => self.health.update(|h| h.node_daa = Some(d.virtual_daa_score)),
            Err(e) => tracing::debug!("status refresh failed: {e}"),
        }
    }

    /// The cursor, or the outcome that stops this step.
    async fn cursor_or_outcome(&self) -> Result<Cursor, StepOutcome> {
        match self.ensure_cursor().await {
            Ok(c) => Ok(c),
            Err(StepOutcome::Gap(r)) => {
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(r.clone());
                });
                Err(StepOutcome::Gap(r))
            }
            Err(o) => {
                if let StepOutcome::Retry(r) = &o {
                    self.set_state(FollowerState::NodeUnavailable, Some(r.clone()));
                }
                Err(o)
            }
        }
    }

    /// One poll of the node and at most one atomic commit.
    ///
    /// With `fetch_parallel` > 1 and the cursor far behind, the batch is the next of the windows
    /// fetched ahead, see [`super::prefetch`].
    pub async fn step(&self) -> StepOutcome {
        // The running time ticks through the step: whatever a node takes is in it (module
        // documentation, *Progress*).
        self.tick(true);
        let outcome = self
            .ticked(async {
                let mut outcome = match self.check_authority().await {
                    Some(o) => o,
                    None if self.cfg.fetch_parallel > 1 => self.step_prefetch().await,
                    None => self.step_sequential().await,
                };
                // The progress rule at every step, whatever the step did: an answer, an empty one,
                // a timeout.
                let cursor = *self.cursor_seen.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(c) = cursor {
                    self.pace(c).await;
                }
                // After a failure, or a period of the progress rule that failed.
                if matches!(outcome, StepOutcome::Retry(_)) || self.rule().failed.is_some() {
                    if let Some(o) = self.hand_over().await {
                        outcome = o;
                    }
                }
                outcome
            })
            .await;
        self.tick(true);
        let nodes = self.source.node_stats();
        let source = self.source.source_status();
        // The alarms of named nodes (`rpc::judge`), process-wide.
        let mut alarms = crate::rpc::judge::alarms();
        let cross = self.source.cross_check();
        if cross.alone {
            alarms.push(format!("cross-check: {}", cross.reason.as_deref().unwrap_or("none answers: the chain authority runs alone")));
        }
        let proof = self.proof.as_ref().map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).status());
        self.health.update(|h| {
            h.cross_checked = cross.checked;
            h.cross_check_alone = cross.alone;
            h.cross_check_reason = cross.reason;
            h.chain_proofs_off = proof.is_none();
            h.chain_proof = proof;
            if !nodes.is_empty() || source.is_some() {
                // Behind a failover the list is the active node's. It may be empty for a single
                // node.
                h.nodes = nodes;
                h.node_source = source;
            }
            h.node_alarms = alarms;
            h.authority_replaced_total = crate::rpc::resolver::authority_replaced() + crate::rpc::multi::promotions();
        });
        outcome
    }

    /// Checks a chain authority not checked yet. One that says it lacks a call stops the follower
    /// (`stopped`), instead of retrying in `node_unavailable` for ever. One that only closes the
    /// connection on it (maybe a flaky link) is warned about and, like one the check could not
    /// tell about, checked again at the next step.
    async fn check_authority(&self) -> Option<StepOutcome> {
        let key = self.source.authority_key();
        if self.checked_authority.lock().unwrap_or_else(|e| e.into_inner()).as_ref() == Some(&key) {
            return None;
        }
        match self.source.check_capabilities(self.proof.is_some()).await {
            crate::rpc::Capable::Yes => {
                *self.checked_authority.lock().unwrap_or_else(|e| e.into_inner()) = Some(key);
                None
            }
            crate::rpc::Capable::Unknown(why) => {
                tracing::debug!(reason = %why, "the chain authority's calls could not be checked yet");
                None
            }
            crate::rpc::Capable::Closed(why) => {
                tracing::warn!(reason = %why, "the chain authority may lack a call the indexer needs (an older node?); not stopping on it, checked again");
                None
            }
            crate::rpc::Capable::No(why) => {
                let fix = if why.contains("getSeqCommitLaneProof") {
                    "run a node of rusty-kaspa v2.1.0 or later (turning [chain_proof] off instead would follow the node's word: no batch proven, and the cursor's time, which the chain authority is judged by, unverified)"
                } else {
                    "run a node of rusty-kaspa v2.1.0 or later"
                };
                let msg = format!("{LACKS_CALL}: {why}; {fix}, then restart");
                self.set_state(FollowerState::Stopped, Some(msg.clone()));
                self.health.update(|h| h.halt_reason = Some(msg.clone()));
                Some(StepOutcome::Halted(msg))
            }
        }
    }

    /// One window from the cursor, fetched now.
    async fn step_sequential(&self) -> StepOutcome {
        let cursor = match self.cursor_or_outcome().await {
            Ok(c) => c,
            Err(o) => return o,
        };
        let plan = match self.plan(cursor.hash).await {
            Ok(p) => p,
            Err(e) => return self.on_rpc_error(e).await,
        };
        let mut confirmations = plan.min_confirmations;
        let mut refetches = 0;
        let (raw, t0, t_fetch, t_check) = loop {
            let mut req = VspcRequest::new(cursor.hash, self.source.authority_verbosity(), confirmations);
            req.timeout_scale = plan.timeout_scale;
            let t0 = std::time::Instant::now();
            let raw = match self.source.vspc_v2(req).await {
                Ok(r) => r,
                Err(e) => {
                    if matches!(e, RpcError::Timeout(_) | RpcError::TooLarge { .. }) {
                        // Split the batch: the same request would only time out, or be too large,
                        // again.
                        let (window, scale) = {
                            let mut w = self.win();
                            w.on_timeout(plan.span);
                            (w.current(), w.timeout_scale())
                        };
                        tracing::warn!(window_blue = window, timeout_scale = scale, "VSPC batch timed out; asking for a smaller one");
                        self.health.update(|h| {
                            h.vspc_timeouts_total += 1;
                            h.batch_window_blue = Some(window);
                        });
                    }
                    return self.on_rpc_error(e).await;
                }
            };
            let t_fetch = t0.elapsed();
            let t_check0 = std::time::Instant::now();
            match self.prove(cursor.hash, raw, Origin::PRIMARY).await {
                Ok(r) => break (r, t0, t_fetch, t_check0.elapsed()),
                // A tip reorg: the chain from the cursor is read again at once, ending below the
                // tip, and the new batch is proven in full.
                Err(o) if is_tip_reorg(&o) && refetches < TIP_REORG_REFETCHES => {
                    refetches += 1;
                    confirmations = Some(confirmations.unwrap_or(0).max(TIP_REORG_CONFIRMATIONS));
                }
                Err(StepOutcome::Retry(m)) if m.starts_with(TIP_REORG) => return self.tip_reorg_wait(m),
                Err(o) => {
                    // The cross-checking node gave no copy of the window's KOB transactions: ask
                    // for a smaller window, as for a timeout of the authority. A fast authority
                    // grows the window, and the copy must not grow with it (R3C1-L1).
                    if matches!(&o, StepOutcome::Retry(m) if m.contains(crate::rpc::multi::WAITING_FOR_CROSS_CHECK)) {
                        let window = {
                            let mut w = self.win();
                            w.on_timeout(plan.span);
                            w.current()
                        };
                        self.health.update(|h| h.batch_window_blue = Some(window));
                    }
                    return o;
                }
            }
        };
        let batch = match tokio::task::spawn_blocking(move || raw.into_batch()).await.expect("parse task panicked") {
            Ok(b) => b,
            Err(e) => {
                let msg = format!("bad VSPC response: {e}");
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                return StepOutcome::Retry(msg);
            }
        };
        let t_parse = t0.elapsed().saturating_sub(t_fetch + t_check);
        // Empty after a tip reorg: the extra confirmations, not the window, held the blocks back.
        if plan.by_window() && batch.added.is_empty() && refetches == 0 {
            self.record_fetch(&batch, t_fetch);
            // The next chain block lies beyond the window, or the stripped head held all of them.
            // Widen it.
            let window = {
                let mut w = self.win();
                w.on_empty();
                w.current()
            };
            self.health.update(|h| {
                h.state = FollowerState::CatchingUp;
                h.batch_window_blue = Some(window);
                h.last_poll_unix_ms = now_ms();
            });
            // An answer; whether it is progress is the progress rule's.
            self.chain_answered();
            return StepOutcome::Widened { window };
        }
        let committed = Committed {
            start: cursor.hash,
            by_window: plan.by_window(),
            t_fetch,
            t_parse,
            t_check,
            end_blue: None,
            prefetched: false,
        };
        self.commit(batch, committed).await
    }

    /// The next window fetched ahead, or a sequential step where parallel fetch does not apply.
    async fn step_prefetch(&self) -> StepOutcome {
        let cursor = match self.cursor_or_outcome().await {
            Ok(c) => c,
            Err(o) => return o,
        };
        let mut pf = self.prefetch.lock().await;
        if pf.active() && pf.head_start() != Some(cursor.hash) {
            // The cursor moved without the windows, as in a rewind, so they no longer continue it.
            pf.discard();
        }
        if !pf.active() {
            let cursor_blue = match self.cursor_blue(cursor.hash).await {
                Ok(Some(b)) => b,
                Ok(None) => return self.step_sequential().await,
                Err(e) => return self.on_rpc_error(e).await,
            };
            let sink_blue = match self.source.sink_blue_score().await {
                Ok(b) => b,
                Err(e) => return self.on_rpc_error(e).await,
            };
            let lag = sink_blue.saturating_sub(cursor_blue);
            self.health.update(|h| h.lag_blue = Some(lag));
            if lag.saturating_sub(self.cfg.min_confirmations.unwrap_or(0)) <= self.cfg.prefetch_min_lag_blue {
                return self.step_sequential().await;
            }
            match self.source.chain_hashes(cursor.hash).await {
                // Less than a first window to the sink: one request from the cursor does better,
                // see `Prefetch::worth_planning`.
                Ok(c) if c.removed_chain_block_hashes.is_empty() && pf.worth_planning(&c.added_chain_block_hashes) => {
                    pf.start(cursor.hash, c.added_chain_block_hashes);
                }
                Err(e) => {
                    self.chain_failed("the chain hashes", &e);
                    return self.step_sequential().await;
                }
                // A reorg at the cursor, or nothing after it: the single step handles each.
                _ => return self.step_sequential().await,
            }
        }
        let t0 = std::time::Instant::now();
        let head = pf.next(&self.source).await;
        let stats = pf.stats;
        drop(pf);
        self.health.update(|h| {
            h.prefetch_windows = stats.windows as u64;
            h.prefetch_in_flight = stats.in_flight as u64;
            h.prefetch_bytes = stats.bytes;
            h.prefetch_bytes_peak = stats.peak_bytes;
            h.prefetch_window_blocks = stats.window_blocks as u64;
            h.prefetch_discards_total = stats.discards;
            h.prefetch_oversize_total = stats.oversize;
        });
        match head {
            Head::Ready { start, raw, sink_blue, end_blue, elapsed, origin } => {
                // A window from another node: what no hash covers is taken from the primary where
                // it can matter, while the proofs run.
                let t_check0 = std::time::Instant::now();
                let (raw, proved, confirmed) = self.check_batch(start, raw, origin, !origin.trusted).await;
                let t_check = t_check0.elapsed();
                if let Err(o) = proved {
                    self.prefetch.lock().await.discard();
                    if is_tip_reorg(&o) {
                        // The plan no longer matches the chain: one step from the cursor.
                        return self.step_sequential().await;
                    }
                    return o;
                }
                if let Err(why) = confirmed {
                    let mut pf = self.prefetch.lock().await;
                    if why.contains(crate::rpc::multi::WAITING_FOR_CROSS_CHECK) {
                        pf.shrink();
                    }
                    pf.discard();
                    drop(pf);
                    tracing::warn!(reason = %why, "window from another node not applied; one step from the cursor");
                    return self.step_sequential().await;
                }
                let waited = t0.elapsed();
                let batch = match tokio::task::spawn_blocking(move || raw.into_batch()).await.expect("parse task panicked") {
                    Ok(b) => b,
                    Err(e) => {
                        self.prefetch.lock().await.discard();
                        let msg = format!("bad VSPC response: {e}");
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                };
                let t_parse = t0.elapsed().saturating_sub(waited);
                // Within `prefetch_min_lag_blue` of the sink after this window: plan nothing more,
                // apply what is fetched or in flight, then take single steps, see
                // `Prefetch::finish`.
                let near_sink = end_blue.is_some_and(|eb| {
                    sink_blue.saturating_sub(eb).saturating_sub(self.cfg.min_confirmations.unwrap_or(0))
                        <= self.cfg.prefetch_min_lag_blue
                });
                if let Some(eb) = end_blue {
                    self.health.update(|h| h.lag_blue = Some(sink_blue.saturating_sub(eb)));
                }
                let committed = Committed { start, by_window: true, t_fetch: elapsed, t_parse, t_check, end_blue, prefetched: true };
                let out = self.commit(batch, committed).await;
                if !matches!(out, StepOutcome::Applied { .. }) {
                    self.prefetch.lock().await.discard();
                } else if near_sink {
                    self.prefetch.lock().await.finish();
                }
                out
            }
            Head::TimedOut { blocks, timeout_scale } => {
                tracing::warn!(blocks, timeout_scale, "prefetched VSPC window timed out; split it");
                self.health.update(|h| h.vspc_timeouts_total += 1);
                let msg = format!("VSPC window timed out (next: {blocks} chain blocks, timeout x{timeout_scale})");
                // The primary's window or its acceptance data for another node's: a secondary that times out is skipped
                // for the next node (`rpc::multi`).
                self.count_chain_failure(msg.clone());
                self.set_state(FollowerState::CatchingUp, None);
                StepOutcome::Retry(msg)
            }
            Head::Discarded(why) => {
                tracing::info!(reason = %why, "prefetched windows dropped; one step from the cursor");
                self.step_sequential().await
            }
        }
    }

    /// The chain proofs of a batch `raw` from `start` (`rpc::proof`), before it is parsed and
    /// applied. A batch that fails is not applied: the node behind `origin` is reported
    /// (`ChainSource::chain_refused`) and the step is retried. A proof the node could not serve
    /// is retried without blame.
    async fn prove(&self, start: Hash32, raw: RawVspcResponse, origin: Origin) -> Result<RawVspcResponse, StepOutcome> {
        let (raw, proved, confirmed) = self.check_batch(start, raw, origin, false).await;
        proved?;
        // The authority's copies of KOB transactions against the cross-checking node's (C-H1).
        confirmed.map_err(|why| {
            let msg = format!("not applied: {why}");
            tracing::warn!(reason = %why, "the chain authority's batch was not applied");
            self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
            StepOutcome::Retry(msg)
        })?;
        Ok(raw)
    }

    /// The comparison of a batch from `origin` with another node: a window from another node with
    /// the primary (`confirm`, [`Follower::confirm_with_primary`]), the chain authority's own with
    /// the cross-checking node ([`Follower::confirm_with_cross`]).
    async fn confirm(&self, start: Hash32, raw: &RawVspcResponse, origin: Origin, confirm: bool) -> Result<(), String> {
        if confirm {
            self.confirm_with_primary(start, raw, origin).await
        } else if origin.trusted {
            self.confirm_with_cross(start, raw).await
        } else {
            Ok(())
        }
    }

    /// The chain proofs of a batch `raw` from `start` ([`Follower::prove`]) and, with `confirm`,
    /// its comparison with the primary ([`Follower::confirm_with_primary`]), run at once.
    ///
    /// Both only read `raw` and ask the primary, so a window from another node waits for the
    /// longer of the two, not for their sum: each is a few round trips, about 0.3 s each to a
    /// remote node. A batch whose local checks (headers, proof of work, ids) fail is not compared.
    /// The first result is that of the proofs, the second that of the comparison (`Ok` when not
    /// asked for).
    async fn check_batch(
        &self,
        start: Hash32,
        raw: RawVspcResponse,
        origin: Origin,
        confirm: bool,
    ) -> (RawVspcResponse, Result<(), StepOutcome>, Result<(), String>) {
        let Some(proof) = self.proof.clone() else {
            let confirmed = self.confirm(start, &raw, origin, confirm).await;
            return (raw, Ok(()), confirmed);
        };
        let p = proof.clone();
        let (raw, plan) = tokio::task::spawn_blocking(move || {
            let r = p.lock().unwrap_or_else(|e| e.into_inner()).check_local(start, &raw);
            (raw, r)
        })
        .await
        .expect("proof task panicked");
        let plan = match plan {
            Ok(p) => p,
            Err(e) => {
                let verdict = self.proof_verdict(&proof, origin, Err(e)).await;
                return (raw, verdict, Ok(()));
            }
        };
        let proved = async {
            match plan {
                None => Ok(()),
                Some(plan) => self.prove_scripts(&proof, &raw, self.prove_lanes(&proof, plan)).await,
            }
        };
        let confirmed = self.confirm(start, &raw, origin, confirm);
        let (proved, confirmed) = tokio::join!(proved, confirmed);
        let verdict = self.proof_verdict(&proof, origin, proved).await;
        (raw, verdict, confirmed)
    }

    /// What a chain proof's result `r` makes of the step: nothing (`Ok`), or a retry after a proof
    /// the node could not serve, or after a refusal that reports the node behind `origin`.
    async fn proof_verdict(&self, proof: &Mutex<ChainProof>, origin: Origin, r: Result<(), ProofError>) -> Result<(), StepOutcome> {
        match r {
            Ok(()) => Ok(()),
            Err(ProofError::Unavailable(why)) => {
                let msg = format!("chain proof unavailable: {why}");
                tracing::warn!(reason = %why, "a chain proof could not be fetched: the batch waits");
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                Err(StepOutcome::Retry(msg))
            }
            Err(ProofError::Reorged(why)) => {
                tracing::debug!(reason = %why, "a block of the batch left the selected parent chain (a tip reorg): fetched again");
                Err(StepOutcome::Retry(format!("{TIP_REORG}: {why}")))
            }
            Err(ProofError::TooDeep(why)) => {
                if let Some(o) =
                    self.wait_unless_synced(&format!("{PROOF_TOO_DEEP} ({why}); nothing is halted until it is synced")).await
                {
                    return Err(o);
                }
                let msg = format!(
                    "{PROOF_TOO_DEEP}: {why}. The cursor is before the node's pruning point, and chain proofs apply no batch unproven: \
                     use a node that still proves it, or rebase the store (kobd db rebase). Turning [chain_proof] off instead would \
                     follow the node's word: no batch proven, and the cursor's time, which the chain authority is judged by, unverified"
                );
                tracing::error!("{msg}");
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                Err(StepOutcome::Gap(msg))
            }
            // The batch's fault, or that of the node that served a proof for it (K-M2).
            Err(ProofError::Refused(why)) => self.refuse_batch(proof, origin, why),
            Err(ProofError::BadProof(from, why)) => self.refuse_batch(proof, from, why),
        }
    }

    /// A batch refused for `why`: the node behind `blamed` is reported.
    fn refuse_batch(&self, proof: &Mutex<ChainProof>, blamed: Origin, why: String) -> Result<(), StepOutcome> {
        proof.lock().unwrap_or_else(|e| e.into_inner()).refused(&why);
        tracing::error!(node = blamed.node, reason = %why, "a batch failed its chain proof: refused, nothing applied, the node is backed off from");
        self.source.chain_refused(blamed, &why);
        let msg = format!("chain proof refused: {why}");
        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
        Err(StepOutcome::Retry(msg))
    }

    /// Stores the floor's targets of the chain proofs, at most once a minute (K-M3).
    async fn save_floor(&self) {
        let Some(s) = self.proof.as_ref().and_then(|p| p.lock().unwrap_or_else(|e| e.into_inner()).floor_due()) else { return };
        let r = self.blocking(move |i| super::db::meta_set(i.conn(), crate::rpc::proof::FLOOR_META_KEY, &s)).await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "chain proofs: the difficulty floor could not be stored");
        }
    }

    /// A step that gave up on a batch after tip reorgs ([`TIP_REORG`]). The node answered, so the
    /// state is not `node_unavailable`. `following` and `catching_up` stay.
    ///
    /// After [`TIP_REORG_STALL_STEPS`] such steps in a row over [`TIP_REORG_STALL`] it is a failure
    /// of the chain authority instead: `node_unavailable`, and the source is told
    /// ([`ChainSource::authority_failing`]: an alarm or another node, a failover).
    fn tip_reorg_wait(&self, msg: String) -> StepOutcome {
        let stall = {
            let mut t = self.tip_reorgs.lock().unwrap_or_else(|e| e.into_inner());
            t.steps += 1;
            let since = *t.since.get_or_insert_with(std::time::Instant::now);
            let long = since.elapsed();
            (t.steps >= TIP_REORG_STALL_STEPS && long >= TIP_REORG_STALL).then(|| {
                let n = std::mem::take(&mut t.steps);
                t.since = None;
                t.stalled = true;
                (n, long)
            })
        };
        if let Some((n, long)) = stall {
            let why = format!(
                "the chain authority claimed a tip reorg in {n} steps in a row over {} s, the cursor does not move ({msg})",
                long.as_secs()
            );
            tracing::warn!(reason = %why, "tip reorgs without end: treated as a failure of the chain authority");
            {
                let mut g = self.chain_failures.lock().unwrap_or_else(|e| e.into_inner());
                g.0 = g.0.max(self.cfg.authority_failures.max(1));
                g.1 = why.clone();
            }
            self.set_state(FollowerState::NodeUnavailable, Some(why.clone()));
            return StepOutcome::Retry(why);
        }
        self.health.update(|h| {
            if matches!(h.state, FollowerState::NodeUnavailable | FollowerState::Starting) {
                h.state = FollowerState::CatchingUp;
            }
            h.last_poll_unix_ms = now_ms();
            h.last_error = Some(msg.clone());
        });
        StepOutcome::Retry(msg)
    }

    /// The node part of the acceptance proof: the anchor's header when it moved, the lanes new at
    /// the anchor, and the lane proofs at the batch's last chain block.
    async fn prove_lanes(&self, proof: &Mutex<ChainProof>, plan: crate::rpc::proof::Plan) -> Result<(), ProofError> {
        let lock = || proof.lock().unwrap_or_else(|e| e.into_inner());
        let unavailable = |what: &str, e: RpcError| {
            if e.left_selected_chain() {
                ProofError::Reorged(format!("{what}: {e}"))
            } else if e.kind() == NodeErrorKind::BlockTooDeep {
                ProofError::TooDeep(format!("{what}: {e}"))
            } else {
                ProofError::Unavailable(format!("{what}: {e}"))
            }
        };
        if lock().needs_anchor(&plan) {
            let (block, from) = self
                .source
                .block(plan.parent, false)
                .await
                .map_err(|e| unavailable("the header of the batch's parent", e))?
                .ok_or_else(|| ProofError::Refused(format!("the node does not know {}, the parent of its own batch", plan.parent)))?;
            lock().set_anchor(&plan, &block.header, from)?;
        }
        let anchor = lock().anchor().map(|a| a.hash).unwrap_or(plan.parent);
        let unknown = lock().unknown_lanes(&plan);
        let mut asked = std::pin::pin!(asked_in_order(unknown.iter().copied(), PROOF_FETCHES, |lane| {
            self.source.lane_proof(anchor, Hash32(lane.as_bytes()))
        }));
        for lane in unknown.iter().copied() {
            let (p, from) =
                asked.next().await.expect("one answer per lane").map_err(|e| unavailable("a lane proof at the cursor", e))?;
            lock().learn_lane(lane, &p, from)?;
        }
        let expected = lock().expected(&plan)?;
        let asks = expected.iter().map(|(lane, _)| self.source.lane_proof(plan.end, Hash32(lane.as_bytes())));
        let proofs = futures_util::future::join_all(asks)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| unavailable("a lane proof at the batch's end", e))?;
        lock().finish(plan, &expected, &proofs)
    }

    /// The signature scripts of the batch's transactions that can matter to KOB
    /// (`super::trust`): each merged block holding one is fetched once and checked
    /// (`ChainProof::check_merged_block`).
    ///
    /// The blocks are asked for [`PROOF_FETCHES`] at a time while `lanes`, the acceptance proof
    /// of the batch, runs, and checked in order once it has passed. One after another, after the
    /// lanes, a busy batch (a block a second holding a KOB transaction on the TN10 soak) waited a
    /// round trip per block and held the follower to about chain speed. A failure of the lanes is
    /// reported before one of the blocks, as when they ran first.
    async fn prove_scripts(
        &self,
        proof: &Mutex<ChainProof>,
        raw: &RawVspcResponse,
        lanes: impl std::future::Future<Output = Result<(), ProofError>>,
    ) -> Result<(), ProofError> {
        let (lanes, fetched) = tokio::join!(lanes, self.merged_blocks(raw));
        lanes?;
        for (m, chain_blocks, txs, (block, from)) in fetched? {
            proof.lock().unwrap_or_else(|e| e.into_inner()).check_merged_block(m, &block, &txs, from)?;
            // In the merge set of the chain block that accepted it (K-H2).
            for b in chain_blocks {
                let chain = crate::rpc::proof::consensus_header(&raw.chain_block_accepted_transactions[b].chain_block_header)
                    .ok_or_else(|| ProofError::Refused(format!("the header of chain block #{b} is incomplete")))?;
                crate::rpc::proof::check_merged_into(&chain, &block.header, &*self.source).await?;
            }
        }
        Ok(())
    }

    /// The merged blocks of the batch's transactions that can matter to KOB, with those
    /// transactions, in the order of their hashes. It stops at the first failure.
    async fn merged_blocks<'a>(&self, raw: &'a RawVspcResponse) -> Result<Vec<MergedBlock<'a>>, ProofError> {
        let keys = super::trust::WindowKeys::of(&raw.chain_block_accepted_transactions);
        let ctx = self
            .blocking(move |i| super::trust::TrustContext::load(i.conn(), i.processor().tokens.clone(), &keys))
            .await
            .map_err(|e| ProofError::Unavailable(format!("database: {e}")))?;
        let blocks = &raw.chain_block_accepted_transactions;
        let mut by_block: std::collections::BTreeMap<Hash32, (Vec<usize>, Vec<&crate::rpc::types::Tx>)> = Default::default();
        for (b, txs) in ctx.matters(blocks).into_iter().enumerate() {
            for k in txs {
                let t = &blocks[b].accepted_transactions[k];
                let Some(m) = t.verbose_data.block_hash else {
                    return Err(ProofError::Refused(format!(
                        "transaction {} has no merged block hash",
                        t.verbose_data.transaction_id
                    )));
                };
                let e = by_block.entry(m).or_default();
                if !e.0.contains(&b) {
                    e.0.push(b);
                }
                e.1.push(t);
            }
        }
        let hashes: Vec<Hash32> = by_block.keys().copied().collect();
        let mut asked = std::pin::pin!(asked_in_order(hashes, PROOF_FETCHES, |m| self.source.block(m, true)));
        let mut out = Vec::with_capacity(by_block.len());
        for (m, (chain_blocks, txs)) in by_block {
            let block = asked
                .next()
                .await
                .expect("one answer per merged block")
                .map_err(|e| ProofError::Unavailable(format!("merged block {m}: {e}")))?
                .ok_or_else(|| ProofError::Refused(format!("the node does not know {m}, the merged block of its own transaction")))?;
            out.push((m, chain_blocks, txs, block));
        }
        Ok(out)
    }

    /// Checks a window `raw` from another node against the primary, as far as it can matter to KOB.
    ///
    /// The window is already checked against the hashes and acceptance data of the primary
    /// (`rpc::verify`). Its chain blocks that hold a transaction that can matter to KOB
    /// (`super::trust`) are fetched from the primary and must agree with it in everything the
    /// indexer reads, signature scripts and spent outputs included. A difference drops the node
    /// (`ChainSource::report_lie`). Any failure leaves the window unapplied, and the `Err` says
    /// why.
    async fn confirm_with_primary(&self, start: Hash32, raw: &RawVspcResponse, origin: Origin) -> Result<(), String> {
        let keys = super::trust::WindowKeys::of(&raw.chain_block_accepted_transactions);
        let ctx = self
            .blocking(move |i| super::trust::TrustContext::load(i.conn(), i.processor().tokens.clone(), &keys))
            .await
            .map_err(|e| format!("database: {e}"))?;
        let flags = ctx.flag(&raw.chain_block_accepted_transactions);
        let runs = flag_runs(&flags, start, &raw.added_chain_block_hashes);
        let flagged = flags.iter().filter(|f| **f).count() as u64;
        self.health.update(|h| {
            h.untrusted_windows_total += 1;
            h.untrusted_blocks_total += flags.len() as u64;
            h.confirmed_blocks_total += flagged;
        });
        if runs.is_empty() {
            return Ok(());
        }
        // The primary's sink once, and the blue score of the end of each run from its checked
        // header.
        let sink_blue = self.source.sink_blue_score().await.map_err(|e| {
            self.chain_failed("the primary's sink", &e);
            format!("the primary's sink: {e}")
        })?;
        let hashes = &raw.added_chain_block_hashes;
        let blocks = &raw.chain_block_accepted_transactions;
        let runs: Vec<crate::rpc::BodyRun> = runs
            .iter()
            .map(|&(prev, first, len)| {
                let last = first + len - 1;
                let req = WindowRequest {
                    min_confirmations: self.cfg.min_confirmations,
                    end_blue: Some(blocks[last].chain_block_header.blue_score),
                    sink_blue: Some(sink_blue),
                    ..WindowRequest::new(prev, hashes[last])
                };
                crate::rpc::BodyRun { req, first, len }
            })
            .collect();
        let answers = futures_util::future::join_all(runs.iter().map(|r| self.source.fetch_window_primary(r.req))).await;
        for (run, answer) in runs.iter().zip(answers) {
            let f = answer.map_err(|e| {
                self.chain_failed("its copy of a window", &e);
                format!("the primary's copy of {} chain blocks: {e}", run.len)
            })?;
            match crate::rpc::run_difference(raw, &f.raw, run).map_err(|e| format!("the primary: {e}"))? {
                None => {}
                Some(d) => {
                    // A third node may decide first (R4C1-L1).
                    self.source.window_contradicted(origin, start, raw, &runs, &d).await;
                    self.health.update(|h| h.lies_total += 1);
                    return Err(format!("node #{} contradicts the primary: {d}", origin.node));
                }
            }
        }
        Ok(())
    }

    /// Checks a window `raw` of the chain authority (`start`) against the cross-checking node, as
    /// far as it can matter to KOB (C-H1).
    ///
    /// The cross-checking node vouches for the chain blocks and their transaction ids
    /// (`rpc::multi`), and the chain proofs for the merged block each KOB transaction names. Neither
    /// covers which copy of a transaction was accepted: a real block may carry a same-id copy with
    /// other signature scripts. So the chain blocks that hold a transaction that can matter
    /// (`super::trust`) are fetched from the cross-checking node too, and must agree in everything the
    /// indexer reads (`ChainSource::confirm_bodies`, which blames on a difference). With no node to
    /// ask (a single node, or the authority alone after `single_after`) it passes.
    async fn confirm_with_cross(&self, start: Hash32, raw: &RawVspcResponse) -> Result<(), String> {
        if !self.source.cross_check().checked {
            // No cross-checking node vouched for this answer (single node, or alone): nobody to ask.
            return Ok(());
        }
        let keys = super::trust::WindowKeys::of(&raw.chain_block_accepted_transactions);
        let ctx = self
            .blocking(move |i| super::trust::TrustContext::load(i.conn(), i.processor().tokens.clone(), &keys))
            .await
            .map_err(|e| format!("database: {e}"))?;
        let flags = ctx.flag(&raw.chain_block_accepted_transactions);
        let blocks = &raw.chain_block_accepted_transactions;
        let runs: Vec<crate::rpc::BodyRun> = flag_runs(&flags, start, &raw.added_chain_block_hashes)
            .into_iter()
            .map(|(prev, first, len)| {
                let last = first + len - 1;
                let req = WindowRequest {
                    min_confirmations: self.cfg.min_confirmations,
                    end_blue: Some(blocks[last].chain_block_header.blue_score),
                    ..WindowRequest::new(prev, raw.added_chain_block_hashes[last])
                };
                crate::rpc::BodyRun { req, first, len }
            })
            .collect();
        if runs.is_empty() {
            return Ok(());
        }
        self.source.confirm_bodies(start, raw, &runs).await.map(|_| ())
    }

    fn record_fetch(&self, batch: &VspcBatch, t_fetch: std::time::Duration) {
        let n_txs: usize = batch.added.iter().map(|b| b.txs.len()).sum();
        let wire_bytes = batch.wire_bytes as u64;
        self.health.update(|h| {
            h.wire_bytes_total += wire_bytes;
            h.txs_fetched_total += n_txs as u64;
            h.last_batch_blocks = batch.added.len() as u64;
            h.last_batch_txs = n_txs as u64;
            h.last_batch_bytes = wire_bytes;
            h.last_fetch_ms = t_fetch.as_millis() as u64;
        });
    }

    /// Applies one batch requested from `c.start` and publishes what it changed.
    async fn commit(&self, batch: VspcBatch, c: Committed) -> StepOutcome {
        self.record_fetch(&batch, c.t_fetch);
        let n_txs: usize = batch.added.iter().map(|b| b.txs.len()).sum();
        let wire_bytes = batch.wire_bytes as u64;
        let last_blue = batch.added.last().map(|b| (b.header.hash, b.header.blue_score));
        // The cursor's time as verified: the proven header's, or with chain proofs off only a
        // header that hashes to its block's hash (R5C1-L2). Else it is read from the block's header
        // (`learn_cursor_time`), and unknown until then.
        let proven = self.proof.is_some();
        let last_ts = batch
            .added
            .last()
            .filter(|b| proven || crate::rpc::verify::header_hash(&b.header) == Some(b.header.hash))
            .map(|b| (b.header.hash, b.header.timestamp));
        // A deposal waiting for its successor: this batch serves chain stamped more than the
        // tolerance after the cursor block and before the deposal, that is, chain that existed and
        // the deposed authority held back. A halted network has none (`Blame`).
        let tol = self.cfg.authority_lag_tolerance.as_millis() as u64;
        let span = self.rule().blame.as_ref().filter(|b| !b.withheld).and_then(|b| b.cursor_ts.map(|c| (c + tol, b.at_ms)));
        let withheld = span.is_some_and(|(lo, hi)| {
            batch.added.iter().any(|b| {
                b.header.timestamp > lo
                    && b.header.timestamp <= hi
                    && (proven || crate::rpc::verify::header_hash(&b.header) == Some(b.header.hash))
            })
        });
        let start = c.start;
        let t1 = std::time::Instant::now();
        let applied = self.blocking(move |i| i.apply_batch(start, &batch)).await;
        let t_commit = t1.elapsed();
        match applied {
            Ok(a) => {
                self.contradiction_unsynced.store(false, std::sync::atomic::Ordering::Relaxed);
                if withheld {
                    if let Some(b) = self.rule().blame.as_mut() {
                        b.withheld = true;
                    }
                }
                let window = {
                    let mut w = self.win();
                    if c.prefetched {
                        w.cursor_moved();
                    } else if a.added > 0 || a.removed_hashes > 0 {
                        w.on_success(c.t_fetch, c.by_window);
                    }
                    w.current()
                };
                let blue = last_blue.filter(|(_, b)| *b > 0).or(c.end_blue.zip(last_blue).map(|(b, (h, _))| (h, b)));
                if let Some((h, b)) = blue.filter(|(h, _)| *h == a.cursor.hash) {
                    *self.cursor_blue.lock().unwrap_or_else(|e| e.into_inner()) = Some((h, b));
                }
                if let Some(t) = last_ts.filter(|(h, _)| *h == a.cursor.hash) {
                    *self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
                }
                let now = now_ms();
                // Per-batch detail is logged at DEBUG (`RUST_LOG=kobd=debug`). The status line
                // every `--status-secs` says how the indexer is doing, as kaspad's "Processed N
                // blocks in the last 10s" does.
                if a.added >= 100 || c.by_window {
                    tracing::debug!(
                        added = a.added,
                        removed = a.removed_hashes,
                        txs = n_txs,
                        relevant = a.relevant_txs,
                        bytes = wire_bytes,
                        window_blue = (c.by_window && !c.prefetched).then_some(window),
                        prefetched = c.prefetched,
                        fetch_ms = c.t_fetch.as_millis() as u64,
                        parse_ms = c.t_parse.as_millis() as u64,
                        check_ms = c.t_check.as_millis() as u64,
                        commit_ms = t_commit.as_millis() as u64,
                        "applied batch"
                    );
                } else {
                    tracing::debug!(
                        added = a.added,
                        removed = a.removed_hashes,
                        fetch_ms = c.t_fetch.as_millis() as u64,
                        check_ms = c.t_check.as_millis() as u64,
                        "applied batch"
                    );
                }
                self.refresh_status().await;
                let snapshot_node_daa = self.health.snapshot().node_daa;
                // A batch that the window ended leaves chain behind it, whatever the node position
                // says.
                let more = c.by_window
                    || snapshot_node_daa.map(|n| n.saturating_sub(a.cursor.daa) > self.cfg.caught_up_daa).unwrap_or(a.added > 0);
                let orders_total =
                    if a.new_orders > 0 || a.reverted_rows_blocks > 0 { self.blocking(|i| i.order_count().ok()).await } else { None };
                let records_next = self.blocking(|i| i.records_next()).await;
                // The update notice is read again only when a placement of the batch announced a
                // newer release.
                if a.release_noticed {
                    if let Ok(Some(n)) = self.blocking(|i| i.newer_release_seen()).await {
                        super::release::publish(&self.health, n);
                    }
                }
                let state = if a.added == 0 || !more { FollowerState::Following } else { FollowerState::CatchingUp };
                // An answer. Whether it is progress is the progress rule's, by the cursor block's
                // time and the local clock alone, whatever the state or the node's position (module
                // documentation, *Progress*): it runs at the end of the step.
                self.chain_answered();
                *self.cursor_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(a.cursor.hash);
                let cursor_time =
                    self.cursor_ts.lock().unwrap_or_else(|e| e.into_inner()).filter(|(h, _)| *h == a.cursor.hash).map(|(_, t)| t);
                self.health.update(|h| {
                    h.state = state;
                    h.cursor_hash = Some(a.cursor.hash);
                    h.cursor_daa = a.cursor.daa;
                    h.cursor_time_unix_ms = cursor_time;
                    if !c.prefetched {
                        h.batch_window_blue = Some(window);
                    }
                    h.last_error = None;
                    h.last_poll_unix_ms = now_ms();
                    // The first poll sets the baseline; the progress rule stamps the rest.
                    if h.last_progress_unix_ms == 0 {
                        h.last_progress_unix_ms = now;
                    }
                    // A commit went through, so there is no storage error any more.
                    h.halt_reason = None;
                    if a.removed_hashes > 0 {
                        h.reorgs_total += 1;
                    }
                    if a.unknown_removed > 0 {
                        h.deep_reorgs_total += 1;
                    }
                    h.reverted_blocks_total += a.reverted_rows_blocks as u64;
                    h.blocks_applied_total += a.added as u64;
                    h.relevant_txs_total += a.relevant_txs;
                    if let Some(t) = orders_total {
                        h.orders_total = t;
                    }
                    if let Some(n) = records_next {
                        h.records_next_n = n;
                    }
                });
                self.note_catch_up();
                self.save_floor().await;
                if !a.event.is_empty() {
                    let _ = self.events.send(Arc::new(a.event));
                }
                if a.added == 0 && a.removed_hashes == 0 {
                    StepOutcome::Idle
                } else {
                    StepOutcome::Applied { added: a.added, removed: a.removed_hashes, more }
                }
            }
            Err(IngestError::EmptyAfterReorg) => {
                let msg = "node reported a reorg with no added blocks; waiting".to_string();
                self.set_state(FollowerState::Following, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
            Err(e @ IngestError::InconsistentRemoved(_)) => {
                let mut msg = format!("stored chain contradicts the node: {e}");
                // A public node that answers inconsistently is given up for another, and the
                // follower asks that one. Once two public nodes on different hosts agree with each
                // other against the store, the store halts (`rpc::multi`).
                match self.source.authority_contradicted(start, &msg) {
                    crate::rpc::StoreContradiction::Replaced => {
                        let msg = format!("{msg}; the public node was given up for another, asking it");
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                    crate::rpc::StoreContradiction::Halt(Some(why)) => msg = format!("{msg}; {why}"),
                    crate::rpc::StoreContradiction::Halt(None) => {}
                }
                // An unsynced node may answer from a chain it is still building: no halt on it.
                let put_off = &self.contradiction_unsynced;
                if let Some(o) = self.wait_unless_synced(&format!("{msg}; nothing is halted until it is synced")).await {
                    put_off.store(true, std::sync::atomic::Ordering::Relaxed);
                    return o;
                }
                // That answer may predate the sync: the batch is fetched again first.
                if put_off.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    let msg = format!("{ASK_AGAIN} {msg}");
                    tracing::info!("{msg}");
                    self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                    return StepOutcome::Retry(msg);
                }
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                StepOutcome::Gap(msg)
            }
            Err(e @ IngestError::Log(_)) => self.halt(&e),
            Err(e) => match super::db::storage_problem(&e) {
                Some(what) => self.storage_error(what, &e),
                None => {
                    let msg = format!("commit failed: {e}");
                    self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                    StepOutcome::Retry(msg)
                }
            },
        }
    }

    async fn on_rpc_error(&self, e: RpcError) -> StepOutcome {
        // Every caller asked the chain authority.
        self.chain_failed("a chain request", &e);
        let mut kind = e.kind();
        if matches!(kind, NodeErrorKind::UnknownHash | NodeErrorKind::RetentionRootMissing) {
            // The store is rewound or halted on this only when the source confirms it.
            let cursor = self.blocking(|i| i.cursor()).await.ok().flatten();
            if let Some(cursor) = cursor {
                match self.source.confirm_unknown_cursor(cursor.hash, &e).await {
                    Ok(k) => kind = k,
                    Err(why) => {
                        let msg = format!(
                            "the chain authority disowns the cursor {}, which is not confirmed ({why}): nothing is rewound or halted",
                            cursor.hash
                        );
                        self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                        return StepOutcome::Retry(msg);
                    }
                }
            }
            match self.still_disowned_when_synced(cursor.map(|c| c.hash), kind).await {
                Ok(k) => kind = k,
                Err(o) => return o,
            }
        }
        match kind {
            NodeErrorKind::UnknownHash => self.recover_unknown_cursor().await,
            NodeErrorKind::RetentionRootMissing => {
                let msg = format!("{PRUNED_GAP} the node's retention: {e}");
                self.health.update(|h| {
                    h.state = FollowerState::Gap;
                    h.gap_reason = Some(msg.clone());
                    h.last_error = Some(msg.clone());
                });
                StepOutcome::Gap(msg)
            }
            NodeErrorKind::TransitionalIbd | NodeErrorKind::BlockTooDeep | NodeErrorKind::Other => {
                let msg = e.to_string();
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                StepOutcome::Retry(msg)
            }
        }
    }

    /// `None` when the node is synced, else the retry that waits for it. A node that is not
    /// synced (reset, re-syncing) may not know the cursor yet, so nothing is rewound or halted on
    /// its word.
    async fn wait_unless_synced(&self, what: &str) -> Option<StepOutcome> {
        let why = match self.source.server_info().await {
            Ok(i) if i.is_synced => return None,
            Ok(_) => format!("{NOT_SYNCED}: {what}"),
            Err(e) => format!("{what}, and whether the node is synced is unknown (getServerInfo: {e})"),
        };
        self.set_state(FollowerState::NodeUnavailable, Some(why.clone()));
        Some(StepOutcome::Retry(why))
    }

    /// The node disowns the cursor `at` (`kind`). It counts only once the node is synced and,
    /// asked again, still disowns it. A cursor the node knows again is followed on, with no gap.
    async fn still_disowned_when_synced(&self, at: Option<Hash32>, kind: NodeErrorKind) -> Result<NodeErrorKind, StepOutcome> {
        let what = "it disowns the cursor; nothing is rewound or halted until it is synced";
        if let Some(o) = self.wait_unless_synced(what).await {
            return Err(o);
        }
        let Some(at) = at else { return Ok(kind) };
        match self.source.chain_hashes(at).await {
            Ok(_) => {
                let msg = format!("{KNOWN_AGAIN} {at}; following on, no gap");
                tracing::info!("{msg}");
                self.set_state(FollowerState::CatchingUp, Some(msg.clone()));
                Err(StepOutcome::Retry(msg))
            }
            Err(e) if matches!(e.kind(), NodeErrorKind::UnknownHash | NodeErrorKind::RetentionRootMissing) => {
                Ok(if e.kind() == kind { kind } else { NodeErrorKind::UnknownHash })
            }
            Err(e) => {
                let msg = format!("asking the synced node for the cursor {at} again: {e}; nothing is rewound or halted");
                self.set_state(FollowerState::NodeUnavailable, Some(msg.clone()));
                Err(StepOutcome::Retry(msg))
            }
        }
    }

    /// The node does not know the cursor: rewinds to the newest stored block it still knows.
    async fn recover_unknown_cursor(&self) -> StepOutcome {
        let limit = self.cfg.max_walkback_blocks as usize;
        let candidates = match self.blocking(move |i| i.stored_blocks_desc(limit)).await {
            Ok(c) => c,
            Err(e) => return StepOutcome::Retry(format!("database: {e}")),
        };
        // Knowledge is monotone along the chain, because a node that knows a block knows its
        // ancestors. So the newest known stored block is found by bisection: O(log n) node calls
        // for a long window.
        let mut found = None;
        if let Some(oldest) = candidates.last() {
            match self.source.block_exists(oldest.1).await {
                Ok(true) => {
                    let (mut lo, mut hi) = (0usize, candidates.len() - 1);
                    while lo < hi {
                        let mid = (lo + hi) / 2;
                        match self.source.block_exists(candidates[mid].1).await {
                            Ok(true) => hi = mid,
                            Ok(false) => lo = mid + 1,
                            Err(e) => return StepOutcome::Retry(format!("getBlock: {e}")),
                        }
                    }
                    found = Some(candidates[lo]);
                }
                Ok(false) => {}
                Err(e) => return StepOutcome::Retry(format!("getBlock: {e}")),
            }
        }
        if let Some((seq, hash, daa)) = found {
            let target = Cursor { hash, daa };
            return match self.blocking(move |i| i.rewind_to(seq, target)).await {
                Ok(a) => {
                    tracing::warn!(to = %hash, reverted = a.reverted_rows_blocks, "cursor unknown to the node: rewound");
                    *self.cursor_seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(hash);
                    self.chain_answered();
                    self.win().cursor_moved();
                    self.health.update(|h| {
                        h.cursor_hash = Some(hash);
                        h.cursor_daa = daa;
                        h.cursor_time_unix_ms = None;
                        h.reverted_blocks_total += a.reverted_rows_blocks as u64;
                        h.reorgs_total += 1;
                    });
                    if !a.event.is_empty() {
                        let _ = self.events.send(Arc::new(a.event));
                    }
                    StepOutcome::Rewound { blocks: a.reverted_rows_blocks }
                }
                Err(e @ IngestError::Log(_)) => self.halt(&e),
                Err(e) => StepOutcome::Retry(format!("rewind failed: {e}")),
            };
        }
        let msg = format!("the node does not know the cursor or any stored block: {PRUNED_GAP} pruning, or the node was reset");
        self.health.update(|h| {
            h.state = FollowerState::Gap;
            h.gap_reason = Some(msg.clone());
            h.last_error = Some(msg.clone());
        });
        StepOutcome::Gap(msg)
    }

    /// Runs until `shutdown` flips to true.
    ///
    /// A gap stops the following for good, with state `gap`, until it does.
    pub async fn run(&self, shutdown: watch::Receiver<bool>) {
        self.run_loop(shutdown, false).await;
    }

    /// Like [`Follower::run`], but a gap that the pruning of the node caused ends the run at once.
    ///
    /// A gap is such a gap when [`is_pruned_gap`] says so. The run does not wait for the shutdown.
    /// It returns the reason, and the state stays `gap`, for the caller to move the store past it
    /// ([`super::recovery`]). It returns `None` on the shutdown, or on a halt that needs the
    /// operator.
    pub async fn run_recovering(&self, shutdown: watch::Receiver<bool>) -> Option<String> {
        self.run_loop(shutdown, true).await
    }

    async fn run_loop(&self, mut shutdown: watch::Receiver<bool>, recover: bool) -> Option<String> {
        let mut failures = 0u32;
        if self.proof.is_none() {
            tracing::warn!("chain proofs are OFF ([chain_proof] enabled = false, or the node serves none): no batch is proven, and the time of the cursor's chain block, which the chain authority is judged by, is the node's word (checked against the block's hash only)");
        }
        loop {
            if *shutdown.borrow() {
                break;
            }
            // A step waits on the node, and a window can take minutes to arrive, so the stop does
            // not wait for it. Dropping a step loses nothing: its commit is one blocking task that
            // runs to its end, and the next start resumes at the cursor.
            let outcome = tokio::select! {
                o = self.step() => o,
                _ = async { let _ = shutdown.wait_for(|s| *s).await; } => break,
            };
            self.lean_tick().await;
            self.disk_tick().await;
            self.quotes_tick().await;
            let delay = match &outcome {
                StepOutcome::Applied { more: true, .. } | StepOutcome::Rewound { .. } => {
                    failures = 0;
                    Duration::ZERO
                }
                // No progress yet, but a different request is due at once. This is bounded, because
                // the window doubles.
                StepOutcome::Widened { .. } => Duration::ZERO,
                // Another node is the chain authority now (the source warned): ask it at once.
                StepOutcome::Handover(r) => {
                    failures = 0;
                    tracing::info!("{r}");
                    Duration::ZERO
                }
                StepOutcome::Applied { .. } | StepOutcome::Idle => {
                    failures = 0;
                    self.cfg.poll_interval
                }
                StepOutcome::Retry(r) => {
                    failures = failures.saturating_add(1);
                    if retry_is_routine(r, failures) {
                        tracing::debug!(attempt = failures, "{r}");
                    } else {
                        tracing::warn!(attempt = failures, "{r}");
                    }
                    if is_tip_reorg(&outcome) || r.starts_with(KNOWN_AGAIN) || r.starts_with(ASK_AGAIN) {
                        // The node answers: poll again, without a growing backoff.
                        self.cfg.poll_interval
                    } else {
                        let d = self.cfg.backoff.saturating_mul(1u32 << failures.min(10).saturating_sub(1));
                        d.min(self.cfg.max_backoff)
                    }
                }
                StepOutcome::Gap(r) => {
                    if recover && is_pruned_gap(r) {
                        return Some(r.clone());
                    }
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 7.4).");
                    Duration::from_secs(5)
                }
                StepOutcome::Halted(r) => {
                    tracing::error!("indexer halted: {r}. See docs/ops/executor.md (Part B, 8).");
                    Duration::from_secs(5)
                }
            };
            self.health.update(|h| h.consecutive_failures = failures);
            if let StepOutcome::Gap(r) | StepOutcome::Halted(r) = &outcome {
                // Stay halted: do not poll the node again until the operator restarts after
                // bootstrapping. Say so again every `HALT_REMIND`, because an unattended executor
                // stays up and idle, and one line at the start is easily missed.
                loop {
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep(HALT_REMIND) => {
                            tracing::error!("indexer still halted, nothing is indexed or traded: {r}. See docs/ops/executor.md (Part B, 7.4 and 8).");
                        }
                    }
                }
                break;
            }
            if delay.is_zero() {
                tokio::task::yield_now().await;
                continue;
            }
            tokio::select! {
                _ = self.ticked(tokio::time::sleep(delay)) => {}
                _ = shutdown.changed() => {}
            }
        }
        self.health.update(|h| {
            if h.state != FollowerState::Gap {
                h.state = FollowerState::Stopped;
            }
        });
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{asked_in_order, retry_is_routine, PROOF_FETCHES};
    use crate::rpc::multi::TIPS_DIFFER;
    use futures_util::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::time::Duration;

    /// The proof requests of a batch: at most `PROOF_FETCHES` at once, the answers in the order
    /// of the requests whatever order they arrive in, and far less time than one after another.
    #[tokio::test(start_paused = true)]
    async fn proof_requests_go_out_together_and_answer_in_order() {
        let (now, peak) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let n = 40usize;
        let t0 = tokio::time::Instant::now();
        let got: Vec<usize> = asked_in_order(0..n, PROOF_FETCHES, |i| {
            let (now, peak) = (&now, &peak);
            async move {
                peak.fetch_max(now.fetch_add(1, SeqCst) + 1, SeqCst);
                // Later requests answer sooner.
                tokio::time::sleep(Duration::from_millis(10 * (n - i) as u64)).await;
                now.fetch_sub(1, SeqCst);
                i
            }
        })
        .collect()
        .await;
        assert_eq!(got, (0..n).collect::<Vec<_>>());
        assert_eq!(peak.load(SeqCst), PROOF_FETCHES);
        // One after another: 8.2 s. At once, about the slowest of each group in flight.
        let one_by_one: u64 = (1..=n as u64).map(|k| 10 * k).sum();
        assert!(t0.elapsed() < Duration::from_millis(one_by_one / 4), "{:?}", t0.elapsed());
        // One request: a round trip, as before.
        let t1 = tokio::time::Instant::now();
        let one: Vec<u8> = asked_in_order([7u8], PROOF_FETCHES, |x| async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            x
        })
        .collect()
        .await;
        assert_eq!((one, t1.elapsed()), (vec![7], Duration::from_millis(300)));
    }

    /// Nothing is asked before the first answer is wanted, and a caller that stops at a failure
    /// (it drops the stream) asks for nothing more.
    #[tokio::test(start_paused = true)]
    async fn a_proof_that_stops_asks_for_nothing_more() {
        let asked = AtomicUsize::new(0);
        let mut s = std::pin::pin!(asked_in_order(0..100usize, 4, |i| {
            let asked = &asked;
            async move {
                asked.fetch_add(1, SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                i
            }
        }));
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), 0, "nothing before the stream is polled");
        assert_eq!(s.next().await, Some(0));
        let after_first = asked.load(SeqCst);
        assert!(after_first <= 5, "{after_first}");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(asked.load(SeqCst), after_first, "an idle stream asks for nothing more");
    }

    /// R2C-L2: a flag every 5th chain block (cheap spam) gives one run each, so a window of
    /// thousands would ask hundreds of cross-check requests. Past `MAX_BODY_RUNS` they are one span.
    #[test]
    fn flagged_runs_are_bounded_per_window() {
        use super::flag_runs;
        use crate::hex::Hash32;
        let hashes: Vec<Hash32> = (0..2000u32).map(|i| Hash32([(i % 251) as u8; 32])).collect();
        let few: Vec<bool> = (0..2000).map(|i| i == 10 || i == 500).collect();
        assert_eq!(flag_runs(&few, Hash32([0xee; 32]), &hashes), vec![(hashes[9], 10, 1), (hashes[499], 500, 1)]);
        let spam: Vec<bool> = (0..2000).map(|i| i % 5 == 2).collect();
        let runs = flag_runs(&spam, Hash32([0xee; 32]), &hashes);
        assert_eq!(runs, vec![(hashes[1], 2, 1996)], "one span from the first flagged block to the last");
    }

    #[test]
    fn differing_tips_warn_only_when_they_repeat() {
        let r = format!("node error: {TIPS_DIFFER} (chain block #0 differs): nothing is indexed until they agree");
        assert!(retry_is_routine(&r, 1) && retry_is_routine(&r, 2));
        assert!(!retry_is_routine(&r, 3));
        assert!(!retry_is_routine("node error: the nodes disagree on the next chain block (x): nothing is indexed", 1));
        assert!(!retry_is_routine("transport: closed", 1));
    }

    /// The follower runs the chain proofs on a real recorded batch (`rpc::proof` fixtures): the
    /// honest batch is applied, a tampered one is refused, nothing of it is applied, and the node
    /// is reported.
    mod chain_proof {
        use super::super::*;
        use crate::rpc::proof::tests::{fixture, Fixture, MAINNET, TN10};
        use crate::rpc::types::{AddressUtxo, ChainHashes, DagInfo, ServerInfo};
        use std::sync::atomic::Ordering::SeqCst;

        struct Recorded {
            fx: Fixture,
            tamper: fn(&mut RawVspcResponse),
            refused: Mutex<Vec<String>>,
            /// The round trip of each proof request (`getBlock`, `getSeqCommitLaneProof`).
            delay: Duration,
            /// Proof requests in flight now, and the most seen at once.
            in_flight: std::sync::atomic::AtomicUsize,
            peak: std::sync::atomic::AtomicUsize,
            /// Lane proofs at the batch's end still to answer that the block left the selected
            /// parent chain (a tip reorg after the batch was served).
            reorgs: std::sync::atomic::AtomicUsize,
            /// The `minConfirmationCount` of each VSPC request.
            asked: Mutex<Vec<Option<u64>>>,
            /// Lane proofs at the cursor answer "too deep" (the node pruned past it).
            too_deep: std::sync::atomic::AtomicBool,
            /// `getServerInfo` says not synced.
            unsynced: std::sync::atomic::AtomicBool,
            /// What the follower told the source of a failing authority.
            failing: Mutex<Vec<String>>,
            /// Who serves the proofs and blocks, and who was blamed for a refused batch.
            proof_origin: Mutex<Origin>,
            blamed: Mutex<Vec<Origin>>,
            /// Lane proofs that do not give the header's commitment.
            bad_proofs: std::sync::atomic::AtomicBool,
        }

        impl Recorded {
            /// One proof request: in flight for the round trip.
            async fn round_trip(&self) {
                use std::sync::atomic::Ordering::SeqCst;
                let now = self.in_flight.fetch_add(1, SeqCst) + 1;
                self.peak.fetch_max(now, SeqCst);
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                self.in_flight.fetch_sub(1, SeqCst);
            }

            fn last(&self) -> (Hash32, u64) {
                let r = self.fx.raw();
                let b = r.chain_block_accepted_transactions.last().unwrap();
                (b.chain_block_header.hash, b.chain_block_header.blue_score)
            }
        }

        impl ChainSource for Recorded {
            async fn vspc_v2(&self, req: VspcRequest) -> Result<RawVspcResponse, RpcError> {
                assert_eq!(req.data_verbosity_level, crate::rpc::types::Verbosity::Full, "the authority is asked at Full");
                self.asked.lock().unwrap().push(req.min_confirmation_count);
                let mut r = self.fx.raw();
                if req.start_hash != self.fx.start {
                    r.added_chain_block_hashes.clear();
                    r.chain_block_accepted_transactions.clear();
                }
                (self.tamper)(&mut r);
                Ok(r)
            }
            async fn dag_info(&self) -> Result<DagInfo, RpcError> {
                let (sink, _) = self.last();
                Ok(DagInfo { network: self.fx.network.clone(), sink, pruning_point_hash: self.fx.start, virtual_daa_score: 0 })
            }
            async fn server_info(&self) -> Result<ServerInfo, RpcError> {
                Ok(ServerInfo {
                    server_version: "2.1.0".into(),
                    network_id: self.fx.network.clone(),
                    is_synced: !self.unsynced.load(SeqCst),
                    has_utxo_index: true,
                    virtual_daa_score: 0,
                })
            }
            async fn utxos_by_addresses(&self, _a: &[String]) -> Result<Vec<AddressUtxo>, RpcError> {
                Ok(vec![])
            }
            async fn block_exists(&self, _h: Hash32) -> Result<bool, RpcError> {
                Ok(true)
            }
            async fn sink_blue_score(&self) -> Result<u64, RpcError> {
                Ok(self.last().1)
            }
            async fn block_blue_score(&self, h: Hash32) -> Result<Option<u64>, RpcError> {
                if h == self.fx.start {
                    return Ok(Some(self.fx.start_header.blue_score));
                }
                Ok(self
                    .fx
                    .raw()
                    .chain_block_accepted_transactions
                    .iter()
                    .find(|b| b.chain_block_header.hash == h)
                    .map(|b| b.chain_block_header.blue_score))
            }
            async fn chain_hashes(&self, _s: Hash32) -> Result<ChainHashes, RpcError> {
                Ok(ChainHashes::default())
            }
            fn proves_chain(&self) -> bool {
                true
            }
            async fn lane_proof(
                &self,
                block: Hash32,
                lane: Hash32,
            ) -> Result<(kaspa_rpc_core::GetSeqCommitLaneProofResponse, Origin), RpcError> {
                self.round_trip().await;
                let from = *self.proof_origin.lock().unwrap();
                let p = self
                    .fx
                    .proofs
                    .get(&kaspa_hashes::Hash::from_bytes(lane.0))
                    .ok_or_else(|| RpcError::Node("no such lane".into()))?;
                let mut p = p.clone();
                if self.bad_proofs.load(SeqCst) {
                    p.1.payload_and_ctx_digest = kaspa_hashes::Hash::from_bytes([7; 32]);
                }
                match block {
                    b if b == self.fx.start && self.too_deep.load(SeqCst) => {
                        Err(RpcError::Node(format!("block {b} is too deep (before the current pruning point)")))
                    }
                    b if b == self.fx.start => Ok((p.0, from)),
                    b if b == self.last().0 && self.reorgs.fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1)).is_ok() => {
                        Err(RpcError::Node(format!("block {b} is not in the selected parent chain")))
                    }
                    b if b == self.last().0 => Ok((p.1, from)),
                    _ => Err(RpcError::Node("not recorded".into())),
                }
            }
            async fn block(&self, hash: Hash32, _txs: bool) -> Result<Option<(kaspa_consensus_core::block::Block, Origin)>, RpcError> {
                self.round_trip().await;
                if hash.0 == self.fx.merged.header.hash.as_bytes() {
                    return Ok(Some((self.fx.merged.clone(), *self.proof_origin.lock().unwrap())));
                }
                Ok((hash == self.fx.start).then(|| {
                    (kaspa_consensus_core::block::Block::new(self.fx.start_header.clone(), vec![]), *self.proof_origin.lock().unwrap())
                }))
            }
            fn chain_refused(&self, origin: Origin, why: &str) {
                self.refused.lock().unwrap().push(why.to_string());
                self.blamed.lock().unwrap().push(origin);
            }
            async fn authority_failing(&self, _at: Hash32, why: &str) -> bool {
                self.failing.lock().unwrap().push(why.to_string());
                false
            }
        }

        fn follower(
            text: &str,
            tamper: fn(&mut RawVspcResponse),
        ) -> (Follower<Recorded>, Arc<Recorded>, Arc<Mutex<Ingest>>, Arc<HealthState>) {
            follower_with(text, tamper, Duration::ZERO)
        }

        fn follower_with(
            text: &str,
            tamper: fn(&mut RawVspcResponse),
            delay: Duration,
        ) -> (Follower<Recorded>, Arc<Recorded>, Arc<Mutex<Ingest>>, Arc<HealthState>) {
            let fx = fixture(text);
            let network = fx.network.clone();
            let start = fx.start;
            let source = Arc::new(Recorded {
                fx,
                tamper,
                refused: Mutex::new(vec![]),
                delay,
                in_flight: Default::default(),
                peak: Default::default(),
                reorgs: Default::default(),
                asked: Mutex::new(vec![]),
                too_deep: Default::default(),
                unsynced: Default::default(),
                failing: Mutex::new(vec![]),
                proof_origin: Mutex::new(Origin::PRIMARY),
                blamed: Mutex::new(vec![]),
                bad_proofs: Default::default(),
            });
            let conn = crate::indexer::db::open_memory(&network).unwrap();
            let ingest =
                Arc::new(Mutex::new(Ingest::new(conn, crate::testkit::processor(), None).with_config(crate::testkit::wide_window())));
            let health = Arc::new(HealthState::new(&network));
            let (events, _) = broadcast::channel(8);
            let mut cfg = FollowerConfig::from_indexer(&crate::config::IndexerConfig { network, ..Default::default() });
            cfg.start = StartMode::Hash(start);
            cfg.fetch_parallel = 1;
            assert!(cfg.chain_proof.is_some(), "on by default");
            let f = Follower::new(source.clone(), ingest.clone(), cfg, health.clone(), events);
            (f, source, ingest, health)
        }

        fn cursor(ingest: &Mutex<Ingest>) -> Hash32 {
            ingest.lock().unwrap().cursor().unwrap().unwrap().hash
        }

        #[tokio::test]
        async fn an_honest_batch_is_applied_and_a_tampered_one_is_refused() {
            for text in [TN10, MAINNET] {
                let (f, src, ingest, health) = follower(text, |_| {});
                let (last, _) = src.last();
                let n = src.fx.raw().added_chain_block_hashes.len();
                let o = f.step().await;
                assert!(matches!(o, StepOutcome::Applied { added, .. } if added == n), "{o:?}");
                assert_eq!(cursor(&ingest), last);
                let p = health.snapshot().chain_proof.unwrap();
                assert_eq!((p.blocks_total, p.refused_total), (n as u64, 0));
                assert!(src.refused.lock().unwrap().is_empty());
                assert!(!health.snapshot().cross_checked, "a single node is not cross-checked");

                // A native transaction hidden by the node: refused, nothing applied, the node reported.
                let (f, src, ingest, health) = follower(text, |r| {
                    let b = r.chain_block_accepted_transactions.iter_mut().find(|b| b.accepted_transactions.len() > 1);
                    if let Some(b) = b {
                        b.accepted_transactions.pop();
                    }
                });
                let o = f.step().await;
                assert!(matches!(&o, StepOutcome::Retry(m) if m.contains("chain proof refused")), "{o:?}");
                assert_eq!(cursor(&ingest), src.fx.start, "nothing applied");
                assert_eq!(src.refused.lock().unwrap().len(), 1);
                let p = health.snapshot().chain_proof.unwrap();
                assert_eq!((p.blocks_total, p.refused_total), (0, 1));
                assert!(health.snapshot().status_line().contains("chain proofs refused 1"));
            }
        }

        /// A lane proof the node can no longer serve at the cursor (`BlockTooDeep`) is no
        /// `node_unavailable` for ever: nothing while the node is not synced, then a gap that a
        /// recovering follower does not move past. Nothing is applied unproven.
        #[tokio::test]
        async fn a_lane_proof_too_deep_halts_once_the_node_is_synced() {
            let (f, src, ingest, health) = follower(TN10, |_| {});
            ingest.lock().unwrap().init_cursor(&Cursor { hash: src.fx.start, daa: 0 }).unwrap();
            src.too_deep.store(true, SeqCst);
            src.unsynced.store(true, SeqCst);
            let o = f.step().await;
            assert!(matches!(&o, StepOutcome::Retry(m) if m.starts_with(NOT_SYNCED) && m.contains(PROOF_TOO_DEEP)), "{o:?}");
            assert_eq!(health.snapshot().state, FollowerState::NodeUnavailable);
            src.unsynced.store(false, SeqCst);
            let o = f.step().await;
            let StepOutcome::Gap(why) = &o else { panic!("{o:?}") };
            assert!(why.starts_with(PROOF_TOO_DEEP) && why.contains("too deep") && !is_pruned_gap(why), "{why}");
            let h = health.snapshot();
            assert_eq!((h.state, h.gap_reason.as_deref()), (FollowerState::Gap, Some(why.as_str())));
            assert_eq!(cursor(&ingest), src.fx.start, "nothing applied");
            assert_eq!(h.chain_proof.unwrap().refused_total, 0, "the node is not blamed");
        }

        /// The proofs of a batch wait one round trip per kind of request, not one per request: the
        /// lanes new at the anchor go out together (two on the mainnet fixture), as the lanes at the
        /// batch's end do, and the merged blocks are fetched meanwhile. One by one, a batch waited a
        /// round trip per lane at the anchor and per merged block, after the lanes.
        #[tokio::test(start_paused = true)]
        async fn the_proof_requests_of_a_batch_go_out_together() {
            let rtt = Duration::from_millis(300);
            let mut merged = 0;
            for text in [MAINNET, TN10] {
                let (f, src, ingest, health) = follower_with(text, |_| {}, rtt);
                let lanes = src.fx.proofs.len();
                let t0 = tokio::time::Instant::now();
                let o = f.step().await;
                let took = t0.elapsed();
                assert!(matches!(o, StepOutcome::Applied { .. }), "{o:?}");
                assert_eq!(cursor(&ingest), src.last().0);
                let p = health.snapshot().chain_proof.unwrap();
                assert_eq!(p.refused_total, 0);
                assert!(src.refused.lock().unwrap().is_empty());
                // The anchor's header, the lanes at the anchor, the lanes at the end. The merged
                // blocks of the transactions that can matter to KOB arrive meanwhile.
                let rounds = 3;
                merged += p.merged_blocks_total;
                assert!(took >= rtt * rounds && took < rtt * (rounds + 1), "{took:?} for {rounds} round trips, {lanes} lanes");
                assert!(src.peak.load(SeqCst) >= lanes, "the lanes were asked together");
            }
            // The mainnet fixture has two lanes: one by one they took a round trip more, and the
            // merged block one more after them.
            assert_eq!(fixture(MAINNET).proofs.len(), 2);
            assert!(merged > 0, "a merged block was checked");
        }

        /// A tip reorg takes the batch's last block off the chain between the batch and its lane
        /// proofs (TN10 soak, 2026-11-18: several times a minute). The batch is fetched again at
        /// once, ending below the tip, and proven in full in the same step. Nothing is refused and
        /// the state never reads `node_unavailable`. Before, the step failed as "chain proof
        /// unavailable", waited out a growing backoff and reported the node unavailable.
        #[tokio::test]
        async fn a_tip_reorg_at_the_batch_end_is_fetched_again_at_once() {
            for text in [TN10, MAINNET] {
                let (f, src, ingest, health) = follower(text, |_| {});
                src.reorgs.store(1, SeqCst);
                let n = src.fx.raw().added_chain_block_hashes.len();
                let o = f.step().await;
                assert!(matches!(o, StepOutcome::Applied { added, .. } if added == n), "{o:?}");
                assert_eq!(cursor(&ingest), src.last().0);
                assert_eq!(*src.asked.lock().unwrap(), vec![None, Some(TIP_REORG_CONFIRMATIONS)]);
                let h = health.snapshot();
                assert_ne!(h.state, FollowerState::NodeUnavailable);
                assert_eq!(h.chain_proof.unwrap().refused_total, 0);
                assert!(src.refused.lock().unwrap().is_empty(), "the node is not blamed");
            }
        }

        /// Tip reorgs that go on past the fetches of one step: the step gives up for one poll.
        /// Nothing is applied or refused, and the state is `catching_up`, not `node_unavailable`.
        /// Once the chain settles, the next step applies the batch.
        #[tokio::test]
        async fn repeated_tip_reorgs_wait_one_poll_without_blaming_the_node() {
            let (f, src, ingest, health) = follower(TN10, |_| {});
            src.reorgs.store(usize::MAX, SeqCst);
            let o = f.step().await;
            assert!(matches!(&o, StepOutcome::Retry(m) if m.starts_with(TIP_REORG)), "{o:?}");
            assert!(retry_is_routine(
                match &o {
                    StepOutcome::Retry(m) => m,
                    _ => unreachable!(),
                },
                1
            ));
            assert_eq!(src.asked.lock().unwrap().len(), 1 + TIP_REORG_REFETCHES as usize);
            assert_eq!(cursor(&ingest), src.fx.start, "nothing applied");
            let h = health.snapshot();
            assert_eq!(h.state, FollowerState::CatchingUp);
            assert!(!h.status_line().contains("waiting for the node"), "{}", h.status_line());
            assert_eq!(h.chain_proof.unwrap().refused_total, 0);
            assert!(src.refused.lock().unwrap().is_empty(), "the node is not blamed");

            src.reorgs.store(0, SeqCst);
            let o = f.step().await;
            assert!(matches!(o, StepOutcome::Applied { .. }), "{o:?}");
            assert_eq!(cursor(&ingest), src.last().0);
        }

        /// K-M2: a lane proof that fails on its own blames the node that served it, not the node
        /// that served the batch.
        #[tokio::test]
        async fn a_bad_proof_blames_the_node_that_served_it() {
            let other = Origin { node: 1, trusted: false, tier: 0 };
            let (f, src, ingest, _health) = follower(TN10, |_| {});
            *src.proof_origin.lock().unwrap() = other;
            src.bad_proofs.store(true, SeqCst);
            let o = f.step().await;
            assert!(matches!(&o, StepOutcome::Retry(m) if m.contains("chain proof refused")), "{o:?}");
            assert_eq!(cursor(&ingest), src.fx.start, "nothing applied");
            assert_eq!(*src.blamed.lock().unwrap(), vec![other], "the proof's server, not the batch's");
            // A batch that its valid proofs contradict blames the batch's node.
            let (f, src, _ingest, _health) = follower(TN10, |r| {
                let b = r.chain_block_accepted_transactions.iter_mut().find(|b| b.accepted_transactions.len() > 1).unwrap();
                b.accepted_transactions.pop();
            });
            *src.proof_origin.lock().unwrap() = other;
            let _ = f.step().await;
            assert_eq!(*src.blamed.lock().unwrap(), vec![Origin::PRIMARY]);
        }

        /// K-M1: a node that answers every lane proof "not in the selected parent chain" no longer
        /// stalls the follower for ever without blame. After `TIP_REORG_STALL_STEPS` steps over
        /// `TIP_REORG_STALL` it is a failure of the chain authority: `node_unavailable`, and the
        /// source is told (an alarm, another node, a failover).
        #[tokio::test]
        async fn endless_tip_reorgs_count_as_a_failing_authority() {
            let (f, src, ingest, health) = follower(TN10, |_| {});
            src.reorgs.store(usize::MAX, SeqCst);
            for _ in 1..TIP_REORG_STALL_STEPS {
                let o = f.step().await;
                assert!(matches!(&o, StepOutcome::Retry(m) if m.starts_with(TIP_REORG)), "{o:?}");
            }
            // Steps alone are not enough: the stall must last.
            let o = f.step().await;
            assert!(matches!(&o, StepOutcome::Retry(m) if m.starts_with(TIP_REORG)), "{o:?}");
            assert!(src.failing.lock().unwrap().is_empty());
            f.tip_reorgs.lock().unwrap().since = std::time::Instant::now().checked_sub(TIP_REORG_STALL);
            let o = f.step().await;
            assert!(matches!(&o, StepOutcome::Retry(m) if m.contains("claimed a tip reorg")), "{o:?}");
            assert_eq!(health.snapshot().state, FollowerState::NodeUnavailable);
            let told = src.failing.lock().unwrap().clone();
            assert_eq!(told.len(), 1, "the source is told once");
            assert!(told[0].contains("claimed a tip reorg"), "{told:?}");
            assert_eq!(cursor(&ingest), src.fx.start, "nothing applied");
            // The cursor moves: the count begins again.
            src.reorgs.store(0, SeqCst);
            assert!(matches!(f.step().await, StepOutcome::Applied { .. }));
            assert_eq!(f.tip_reorgs.lock().unwrap().steps, 0);
        }
    }
}

#[cfg(test)]
#[path = "pace_tests.rs"]
mod pace_tests;
