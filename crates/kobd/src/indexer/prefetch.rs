//! Parallel window prefetch for a follower that is far behind the sink.
//!
//! The follower fetches several upcoming VSPC windows at once, each on its own connection, and
//! applies them strictly in chain order. docs/ops/executor.md, Part B 3, "Parallel fetch: catching
//! up over several connections", has the operator view and the measurements.
//!
//! # Plan and windows
//!
//! A VSPC request starts at a chain block, so a window that does not start at the cursor needs the
//! hash of the chain block it starts after. The follower first asks the node for the selected
//! chain as hashes only (`getVirtualChainFromBlock` without transaction ids). It cuts that chain
//! into windows of whole chain blocks `(start, end]`. Each window is fetched as
//! `getVirtualChainFromBlockV2(start, High)`, bounded through `minConfirmationCount` to end at
//! `end`. The sink moves on while the request travels, so blocks past `end` may come back, and
//! they are cut off.
//!
//! A response is used only if it continues the planned chain exactly. It has no removed blocks,
//! and its added hashes are a prefix of the window's. Anything else discards the window and every
//! later one: a reorg moved the chain under the plan, or the node forgot a block. The follower
//! then takes a single step from its cursor, which handles the reorg. A shorter response, cut by
//! the node because of merged blocks, is applied and the rest of the window is fetched again.
//!
//! The selected-parent ancestry of the later chain block fixes the chain between two chain
//! blocks. Windows fetched at different times therefore still join into one selected chain, as
//! long as each one starts on the chain its predecessor ended on. The cursor check of the commit
//! (`Ingest::apply_batch`) holds the windows to that.
//!
//! # Window size
//!
//! The batch-size rules of `rpc::window` apply per window, counted in chain blocks:
//! - A window that times out is split in half. A single chain block gets a longer timeout
//!   instead, up to 8x.
//! - The window size halves after a fetch slower than the target.
//! - It doubles after a full-size fetch faster than half of the target.
//!
//! # The end of the plan
//!
//! The plan ends at the sink. Whenever the planned chain runs out, the plan asks for the chain
//! after its tail. An extension that holds fewer chain blocks than a first window
//! (`initial_blocks`) means that the tail has reached the sink. Those chain blocks are not
//! planned, the queued windows are applied, and the follower takes single steps from there: one
//! request from the cursor to the sink. It does the same once an applied window leaves it within
//! `prefetch_min_lag_blue` of the sink ([`Prefetch::finish`]).
//!
//! A plan that kept extending at the sink would cut windows of the one or two chain blocks that
//! arrived since its tail. On a live chain every extension finds some. They would be applied one
//! per step, at most `parallel` per round trip, which is slower than the chain on a remote link.
//! The follower would never get within the threshold again.
//!
//! # Memory
//!
//! `prefetch_max_bytes` is a hard bound. Windows fetched and not yet applied count with their
//! size on the wire, and windows in flight count with the allowance they were sent with. A window
//! is sent only while the sum stays within the budget.
//!
//! The allowance is the estimate of the window: the wire bytes per chain block times its chain
//! blocks, plus a quarter. It is also the size limit of the request (`VspcRequest::max_bytes`).
//! The transport refuses a larger answer as its frame header arrives, before buffering it
//! ([`RpcError::TooLarge`]). A source that ignores the limit has its oversized answer dropped the
//! same way.
//!
//! An oversized window raises the estimate per chain block to what it reported, at once, so the
//! windows that follow are smaller. The window is split in half. A single chain block is sent
//! again with an allowance of its reported size.
//!
//! The window at the cursor always goes. It gets the room left. When that is too little, the
//! windows behind it are dropped and fetched again later, so it gets the whole budget. The one
//! excess left is a single chain block larger than the whole budget. It is fetched alone, with
//! nothing else held.
//!
//! `prefetch_bytes_peak` reports the largest sum seen, and `prefetch_oversize_total` counts the
//! refused answers.

use crate::hex::Hash32;
use crate::rpc::types::RawVspcResponse;
pub use crate::rpc::Fetched;
use crate::rpc::{ChainSource, Origin, RpcError, WindowRequest};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

enum State {
    Queued,
    Running(AbortHandle),
    Done(Result<Fetched, RpcError>),
}

struct Slot {
    id: u64,
    /// The chain block the window starts after (the cursor, for the window at the head).
    start: Hash32,
    /// The window's chain blocks in order; never empty.
    blocks: Vec<Hash32>,
    timeout_scale: u32,
    /// Bytes reserved while it is in flight: the request's size limit.
    allowance: u64,
    /// The size an earlier request of exactly this window reported when it was refused.
    need: u64,
    /// A single chain block larger than the whole budget: sent without a size limit, alone.
    unbounded: bool,
    state: State,
}

impl Slot {
    fn new(id: u64, start: Hash32, blocks: Vec<Hash32>, timeout_scale: u32) -> Slot {
        Slot { id, start, blocks, timeout_scale, allowance: 0, need: 0, unbounded: false, state: State::Queued }
    }

    /// Bytes the window holds: its answer once fetched, its allowance in flight, else nothing.
    fn bytes(&self) -> u64 {
        match &self.state {
            State::Done(Ok(f)) => f.raw.wire_bytes as u64,
            State::Done(Err(_)) | State::Queued => 0,
            State::Running(_) => self.allowance,
        }
    }
}

/// Settings of the prefetch, taken from `FollowerConfig`.
#[derive(Debug, Clone, Copy)]
pub struct PrefetchLimits {
    /// Windows in flight at once.
    pub parallel: usize,
    /// Memory budget in wire bytes, a hard bound.
    pub max_bytes: u64,
    /// A window should take about this long to fetch.
    pub target: Duration,
    /// Chain blocks of the first window.
    pub initial_blocks: usize,
    /// `minConfirmationCount` the caller wants anyway.
    pub min_confirmations: Option<u64>,
}

/// What the head of the queue holds once it is ready.
pub enum Head {
    /// The window at the cursor, checked against the plan and cut to it.
    Ready { start: Hash32, raw: RawVspcResponse, sink_blue: u64, end_blue: Option<u64>, elapsed: Duration, origin: Origin },
    /// The plan was dropped: the response does not continue the planned chain, or the node failed.
    Discarded(String),
    /// The window timed out and was split, or given a longer timeout.
    TimedOut { blocks: usize, timeout_scale: u32 },
}

/// Counters for `/v1/health`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PrefetchStats {
    /// Windows queued, in flight or fetched.
    pub windows: usize,
    /// Windows in flight.
    pub in_flight: usize,
    /// Bytes held now: the fetched answers and the allowances of the windows in flight.
    pub bytes: u64,
    /// The largest `bytes` seen.
    pub peak_bytes: u64,
    /// Current window size in chain blocks.
    pub window_blocks: usize,
    /// Plans dropped because the chain moved.
    pub discards: u64,
    /// Windows that timed out.
    pub timeouts: u64,
    /// Answers refused as larger than their allowance.
    pub oversize: u64,
}

/// Plans, fetches and orders the windows ahead of a follower's cursor.
pub struct Prefetch {
    lim: PrefetchLimits,
    slots: VecDeque<Slot>,
    /// Planned chain not yet cut into windows, continuing from `tail`.
    skeleton: VecDeque<Hash32>,
    tail: Option<Hash32>,
    /// The node had no more chain after `tail`, at its sink or after a reorg. Nothing more is
    /// planned until the queue drains.
    exhausted: bool,
    next_id: u64,
    /// Current window size in chain blocks.
    k: usize,
    /// Wire bytes per chain block, smoothed.
    bytes_per_block: Option<f64>,
    tx: mpsc::UnboundedSender<(u64, Result<Fetched, RpcError>)>,
    rx: mpsc::UnboundedReceiver<(u64, Result<Fetched, RpcError>)>,
    /// Counters for `/v1/health`.
    pub stats: PrefetchStats,
}

impl Prefetch {
    /// Creates an idle prefetch with the given limits.
    pub fn new(lim: PrefetchLimits) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let k = lim.initial_blocks.max(1);
        Prefetch {
            lim,
            slots: VecDeque::new(),
            skeleton: VecDeque::new(),
            tail: None,
            exhausted: false,
            next_id: 1,
            k,
            bytes_per_block: None,
            tx,
            rx,
            stats: PrefetchStats { window_blocks: k, ..Default::default() },
        }
    }

    /// Whether windows are queued, in flight or fetched, or planned chain is not yet cut.
    pub fn active(&self) -> bool {
        !self.slots.is_empty() || !self.skeleton.is_empty()
    }

    /// The chain block the next window to apply starts after.
    pub fn head_start(&self) -> Option<Hash32> {
        self.slots.front().map(|s| s.start).or(if self.skeleton.is_empty() { None } else { self.tail })
    }

    /// Whether a plan along `chain`, the node's selected chain after the cursor, pays off.
    ///
    /// It does when the chain holds at least a first window. A shorter chain means the cursor is at
    /// the sink in the plan's terms, and one request from it does better.
    pub fn worth_planning(&self, chain: &[Hash32]) -> bool {
        chain.len() >= self.lim.initial_blocks.max(1)
    }

    /// Stops planning because the cursor is near the sink.
    ///
    /// The windows not sent yet are dropped, together with any window behind them. The windows
    /// fetched or in flight are applied, and then the follower takes single steps.
    pub fn finish(&mut self) {
        if let Some(i) = self.slots.iter().position(|s| matches!(s.state, State::Queued)) {
            self.truncate(i);
        } else {
            self.skeleton.clear();
            self.exhausted = true;
        }
        self.refresh_stats();
    }

    /// Plans from `cursor` along `chain`, the node's selected chain after it, as hashes.
    pub fn start(&mut self, cursor: Hash32, chain: Vec<Hash32>) {
        self.reset();
        self.tail = Some(cursor);
        self.skeleton = chain.into();
    }

    /// Drops every window and the plan. Windows in flight are aborted.
    pub fn reset(&mut self) {
        for s in self.slots.drain(..) {
            if let State::Running(h) = s.state {
                h.abort();
            }
        }
        self.skeleton.clear();
        self.tail = None;
        self.exhausted = false;
        while self.rx.try_recv().is_ok() {}
        self.refresh_stats();
    }

    /// Resets because the plan no longer matches the chain, and counts it if a plan was active.
    pub fn discard(&mut self) {
        if self.active() {
            self.stats.discards += 1;
        }
        self.reset();
    }

    fn running(&self) -> usize {
        self.slots.iter().filter(|s| matches!(s.state, State::Running(_))).count()
    }

    fn bytes(&self) -> u64 {
        self.slots.iter().map(Slot::bytes).sum()
    }

    fn refresh_stats(&mut self) {
        let bytes = self.bytes();
        self.stats.windows = self.slots.len();
        self.stats.in_flight = self.running();
        self.stats.bytes = bytes;
        self.stats.peak_bytes = self.stats.peak_bytes.max(bytes);
        self.stats.window_blocks = self.k;
    }

    /// Bytes a window of `blocks` chain blocks is expected to take, with a quarter on top.
    ///
    /// Blocks differ in size. The estimate is the allowance of the window, and a larger answer is
    /// refused and the window split.
    fn estimate(&self, blocks: usize) -> Option<u64> {
        self.bytes_per_block.map(|b| (1.25 * b * blocks as f64).ceil() as u64)
    }

    /// Chain blocks of the next window: the adaptive size, small enough for `parallel` to fit.
    fn window_blocks(&self) -> usize {
        let mut k = self.k;
        if let Some(b) = self.bytes_per_block.filter(|b| *b > 0.0) {
            let fit = (self.lim.max_bytes as f64 / (self.lim.parallel.max(1) as f64 * 1.25 * b)).floor() as usize;
            k = k.min(fit.max(1));
        }
        k.max(1)
    }

    /// Sends the queued window at `i` with `allowance` bytes reserved.
    ///
    /// The allowance is the size limit of the request, and an unbounded window has none.
    fn launch<S: ChainSource>(&mut self, i: usize, allowance: u64, src: &Arc<S>) {
        let s = &mut self.slots[i];
        s.allowance = allowance;
        let max = (!s.unbounded).then(|| usize::try_from(allowance).unwrap_or(usize::MAX));
        let (id, start, end, scale) = (s.id, s.start, *s.blocks.last().expect("a window is never empty"), s.timeout_scale);
        let (src, tx, extra) = (src.clone(), self.tx.clone(), self.lim.min_confirmations);
        let task = tokio::spawn(async move {
            // One window `(start, end]` at `High`, or checked `Full` from another node. It is
            // bounded to end at `end`.
            let w = WindowRequest { timeout_scale: scale, min_confirmations: extra, max_bytes: max, ..WindowRequest::new(start, end) };
            let r = src.fetch_window(w).await;
            let _ = tx.send((id, r));
        });
        s.state = State::Running(task.abort_handle());
    }

    /// The allowance of the queued window at `i` if it may go now.
    ///
    /// A window behind the head goes with its estimate, at least the size it reported before, while
    /// that fits the room the budget leaves.
    ///
    /// The head always goes. It gets its estimate, else the room left. If the room is too little or
    /// the head is known to need more, the windows behind it are dropped and it gets the whole
    /// budget. A head known to need more than the whole budget is split first (`None`). A single
    /// chain block of that size goes unbounded.
    fn admit(&mut self, i: usize) -> Option<u64> {
        let max = self.lim.max_bytes.max(1);
        let s = &self.slots[i];
        let want = self.estimate(s.blocks.len()).unwrap_or(0).max(s.need);
        let room = max.saturating_sub(self.bytes());
        if i > 0 {
            return (want > 0 && want <= room).then_some(want);
        }
        if s.need > max {
            if s.blocks.len() > 1 {
                self.split(0);
                return None;
            }
            // One chain block larger than the whole budget goes alone, without a limit. It is the
            // only excess.
            self.truncate(1);
            self.slots[0].unbounded = true;
            return Some(max);
        }
        if want == 0 {
            // Nothing is known yet, for the first window: it gets the room left.
            return (room > 0).then_some(room).or_else(|| {
                self.truncate(1);
                Some(max)
            });
        }
        if want <= room {
            return Some(want);
        }
        // The head gets the room left, unless that is less than what it is known to need or than
        // half its estimate.
        if s.need == 0 && room >= want / 2 {
            return Some(room);
        }
        self.truncate(1);
        Some(want.min(max))
    }

    fn push_window(&mut self, front: bool, start: Hash32, blocks: Vec<Hash32>, timeout_scale: u32) {
        let slot = Slot::new(self.next_id, start, blocks, timeout_scale);
        self.next_id += 1;
        if front {
            self.slots.push_front(slot);
        } else {
            self.slots.push_back(slot);
        }
    }

    /// Starts queued windows and plans new ones while connections and the memory budget allow.
    async fn fill<S: ChainSource>(&mut self, src: &Arc<S>) {
        loop {
            if self.running() >= self.lim.parallel.max(1) {
                break;
            }
            if let Some(i) = self.slots.iter().position(|s| matches!(s.state, State::Queued)) {
                match self.admit(i) {
                    Some(allowance) => {
                        // Admitting the head may drop the windows behind it. The head stays at 0.
                        self.launch(i, allowance, src);
                        continue;
                    }
                    // The head was split, so admit its first half next. Any other window waits for
                    // room.
                    None if i == 0 => continue,
                    None => break,
                }
            }
            if self.slots.len() >= 4 * self.lim.parallel.max(1) {
                break;
            }
            if self.skeleton.is_empty() {
                let Some(tail) = self.tail.filter(|_| !self.exhausted && !self.slots.is_empty()) else { break };
                match src.chain_hashes(tail).await {
                    // Less than a first window after the tail: the plan has reached the sink.
                    Ok(c) if c.removed_chain_block_hashes.is_empty() && !self.worth_planning(&c.added_chain_block_hashes) => {
                        self.exhausted = true;
                        break;
                    }
                    Ok(c) if c.removed_chain_block_hashes.is_empty() => {
                        self.skeleton = c.added_chain_block_hashes.into();
                    }
                    // `tail` left the selected chain, so cut the plan at the fork.
                    Ok(c) if !c.removed_chain_block_hashes.is_empty() => {
                        self.fork_cut(&c.removed_chain_block_hashes);
                        break;
                    }
                    // At the sink, or the node failed: plan nothing more until the queue drains.
                    _ => {
                        self.exhausted = true;
                        break;
                    }
                }
            }
            let k = self.window_blocks().min(self.skeleton.len());
            if !self.slots.is_empty() {
                // The window at the cursor always goes. Any other goes only within the budget, and
                // once sizes are known.
                match self.estimate(k) {
                    Some(est) if self.bytes() + est <= self.lim.max_bytes => {}
                    _ => break,
                }
            }
            let start = self.tail.expect("a plan has a tail");
            let blocks: Vec<Hash32> = self.skeleton.drain(..k).collect();
            self.tail = blocks.last().copied();
            self.push_window(false, start, blocks, 1);
        }
        self.refresh_stats();
    }

    fn on_message(&mut self, id: u64, r: Result<Fetched, RpcError>) {
        let Some(i) = self.slots.iter().position(|s| s.id == id) else { return };
        if !matches!(self.slots[i].state, State::Running(_)) {
            return;
        }
        let allowance = self.slots[i].allowance;
        let r = match r {
            // A source that ignores the size limit of the request has its answer dropped, as if the
            // transport had refused it.
            Ok(f) if !self.slots[i].unbounded && f.raw.wire_bytes as u64 > allowance => {
                Err(RpcError::TooLarge { size: f.raw.wire_bytes, limit: usize::try_from(allowance).unwrap_or(usize::MAX) })
            }
            r => r,
        };
        // Learn the size of chain blocks from every answer, at once when they grew. A refused
        // answer reports its size.
        let blocks = self.slots[i].blocks.len();
        let seen = match &r {
            Ok(f) => Some(f.raw.wire_bytes as f64 / f.raw.added_chain_block_hashes.len().max(1) as f64),
            Err(RpcError::TooLarge { size, .. }) => Some(*size as f64 / blocks as f64),
            Err(_) => None,
        };
        if let Some(b) = seen.filter(|b| self.bytes_per_block.is_none_or(|old| *b > old)) {
            self.bytes_per_block = Some(b);
        }
        self.slots[i].state = State::Done(r);
    }

    /// Splits the window at `i` in place after a timeout or an answer too large for it.
    ///
    /// The result is two halves. A single chain block gets a longer timeout after a timeout, or an
    /// allowance of the size it reported after an answer that was too large.
    fn split(&mut self, i: usize) -> (usize, u32) {
        let s = self.slots.remove(i).expect("index");
        let too_large = match &s.state {
            State::Done(Err(RpcError::TooLarge { size, .. })) => Some(*size as u64),
            _ => None,
        };
        match (&s.state, too_large) {
            (_, Some(_)) => self.stats.oversize += 1,
            // A queued head known to need more than the whole budget, see `admit`.
            (State::Queued, None) => {}
            _ => self.stats.timeouts += 1,
        }
        let n = s.blocks.len();
        if n > 1 {
            self.k = self.k.min(n / 2).max(1);
            let (a, b) = s.blocks.split_at(n / 2);
            let mid = *a.last().expect("non-empty half");
            let first = Slot::new(self.next_id, s.start, a.to_vec(), 1);
            let second = Slot::new(self.next_id + 1, mid, b.to_vec(), 1);
            self.next_id += 2;
            self.slots.insert(i, second);
            self.slots.insert(i, first);
            (n / 2, 1)
        } else {
            self.k = 1;
            let (scale, need) = match too_large {
                Some(size) => (s.timeout_scale, size.max(s.need)),
                None => ((s.timeout_scale * 2).min(8), s.need),
            };
            let mut slot = Slot::new(self.next_id, s.start, s.blocks, scale);
            slot.need = need;
            self.next_id += 1;
            self.slots.insert(i, slot);
            (1, scale)
        }
    }

    /// Drops the window at `i` and every later one, and plans nothing more until the queue drains.
    fn truncate(&mut self, i: usize) {
        for s in self.slots.drain(i..) {
            if let State::Running(h) = s.state {
                h.abort();
            }
        }
        self.skeleton.clear();
        self.tail = self.slots.back().and_then(|s| s.blocks.last().copied());
        self.exhausted = true;
    }

    /// Cuts the plan at a fork after the node reported `removed` chain blocks, a reorg.
    ///
    /// Every window that holds or starts after a removed block lies past the fork and is dropped,
    /// fetched or not. The windows before the fork stay.
    fn fork_cut(&mut self, removed: &[Hash32]) {
        let gone: std::collections::HashSet<&Hash32> = removed.iter().collect();
        let first = self.slots.iter().position(|s| gone.contains(&s.start) || s.blocks.iter().any(|b| gone.contains(b)));
        if let Some(i) = first {
            self.stats.discards += 1;
            self.truncate(i);
        } else {
            self.skeleton.clear();
            self.exhausted = true;
        }
    }

    /// Handles the windows that came back.
    ///
    /// - A window too large for its allowance, the head too, is split at once.
    /// - Behind the head, a window that reports a reorg cuts the plan at the fork.
    /// - Behind the head, a window that timed out is split at once.
    /// - Behind the head, any other error drops that window and every later one, because the plan
    ///   past it is unproven.
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.slots.len() {
            match &self.slots[i].state {
                State::Done(Err(RpcError::TooLarge { .. })) => {
                    self.split(i);
                    i += 1;
                }
                // The other outcomes of the head are handled by `next`.
                _ if i == 0 => i += 1,
                State::Done(Ok(f)) if !f.raw.removed_chain_block_hashes.is_empty() => {
                    let removed = f.raw.removed_chain_block_hashes.clone();
                    self.fork_cut(&removed);
                    // The head may be past the fork too. The caller sees the queue as it is now.
                    break;
                }
                State::Done(Err(RpcError::Timeout(_))) => {
                    self.split(i);
                    i += 1;
                }
                State::Done(Err(_)) => {
                    self.truncate(i);
                    break;
                }
                _ => i += 1,
            }
        }
    }

    /// Waits for the window at the head, keeping the others going meanwhile.
    ///
    /// Call it only while [`Prefetch::active`].
    pub async fn next<S: ChainSource>(&mut self, src: &Arc<S>) -> Head {
        loop {
            while let Ok((id, r)) = self.rx.try_recv() {
                self.on_message(id, r);
            }
            self.reap();
            self.fill(src).await;
            if self.slots.is_empty() {
                self.discard();
                return Head::Discarded("nothing planned".into());
            }
            if matches!(self.slots[0].state, State::Done(_)) {
                break;
            }
            match self.rx.recv().await {
                Some((id, r)) => self.on_message(id, r),
                None => unreachable!("the sender lives in self"),
            }
        }
        let head = self.slots.pop_front().expect("checked");
        let State::Done(r) = head.state else { unreachable!("checked") };
        let f = match r {
            Ok(f) => f,
            Err(e @ RpcError::Timeout(_)) => {
                self.slots.push_front(Slot { state: State::Done(Err(e)), ..head });
                let (blocks, timeout_scale) = self.split(0);
                self.refresh_stats();
                return Head::TimedOut { blocks, timeout_scale };
            }
            Err(e) => {
                self.discard();
                return Head::Discarded(e.to_string());
            }
        };
        let Fetched { mut raw, sink_blue, end_blue, elapsed, origin } = f;
        let planned = &head.blocks;
        let n = raw.added_chain_block_hashes.len().min(planned.len());
        if !raw.removed_chain_block_hashes.is_empty() || n == 0 || raw.added_chain_block_hashes[..n] != planned[..n] {
            let why = if !raw.removed_chain_block_hashes.is_empty() {
                format!("the node reports {} removed chain blocks after {}", raw.removed_chain_block_hashes.len(), head.start)
            } else if n == 0 {
                format!("no chain block after {}", head.start)
            } else {
                format!("the chain after {} differs from the plan", head.start)
            };
            self.discard();
            return Head::Discarded(why);
        }
        raw.added_chain_block_hashes.truncate(n);
        raw.chain_block_accepted_transactions.truncate(n);
        // Learn the size and speed of a window. Sizes rise at once on arrival, in `on_message`, and
        // settle down again here.
        let per_block = raw.wire_bytes as f64 / n as f64;
        self.bytes_per_block = Some(match self.bytes_per_block {
            Some(b) => 0.7 * b + 0.3 * per_block,
            None => per_block,
        });
        if n == planned.len() {
            if elapsed > self.lim.target {
                self.k = (self.k / 2).max(1);
            } else if elapsed < self.lim.target / 2 && n >= self.k {
                self.k = self.k.saturating_mul(2).min(1 << 16);
            }
        } else {
            // The node cut the answer short because of its merged-block limit. Fetch the rest next.
            self.push_window(true, planned[n - 1], planned[n..].to_vec(), 1);
        }
        self.refresh_stats();
        let end_blue = (n == planned.len()).then_some(end_blue);
        Head::Ready { start: head.start, raw, sink_blue, end_blue, elapsed, origin }
    }
}
