//! Parallel window prefetch: while the follower is far behind, it fetches several upcoming VSPC windows at once, each on
//! its own connection, and applies them strictly in chain order.
//!
//! A VSPC request starts at a chain block, so a window that does not start at the cursor needs the hash of the chain block
//! it starts after. The follower first asks the node for the selected chain as hashes only (`getVirtualChainFromBlock`
//! without transaction ids: up to 2,480 chain blocks for ~170 KB) and cuts it into windows of whole chain blocks
//! `(start, end]`. Each window is fetched as `getVirtualChainFromBlockV2(start, High)` bounded through
//! `minConfirmationCount` to end at `end` (the sink moves on while the request travels, so blocks past `end` may come
//! back; they are cut off). A response is used only if it continues the planned chain exactly: no removed blocks, its
//! added hashes a prefix of the window's. Anything else (a reorg moved the chain under the plan, the node forgot a block)
//! discards the window and every later one; the follower then takes a single step from its cursor, which handles the
//! reorg as before. A shorter response (the node cut it by merged blocks) is applied and the rest of the window
//! fetched again.
//!
//! The chain between two chain blocks is fixed by the later one's selected-parent ancestry, so windows fetched at
//! different times still join into one selected chain as long as each one starts on the chain its predecessor ended on;
//! the cursor check of the commit (`Ingest::apply_batch`) holds the windows to that.
//!
//! Per window, the batch-size rules of `rpc::window` apply in chain blocks: a window that times out is split in half (a
//! single chain block gets a longer timeout instead, up to 8x), the window size halves after a fetch slower than the
//! target and doubles after one faster than half of it.
//!
//! Memory is bounded by `prefetch_max_bytes`, hard: windows fetched and not yet applied count with their size on the wire,
//! windows in flight with the allowance they were sent with, and a window is only sent while the sum stays within the
//! budget. The allowance is the window's estimate (wire bytes per chain block times its chain blocks, plus a quarter) and is
//! the request's own size limit (`VspcRequest::max_bytes`): the transport refuses a larger answer as its frame header
//! arrives, before buffering it ([`RpcError::TooLarge`]; a source that ignores the limit has its oversized answer dropped
//! the same way). An oversized window raises the estimate per chain block to what it reported at once (the windows that
//! follow are smaller) and is split in half, or, a single chain block, sent again with an allowance of its reported size.
//! The window at the cursor always goes: with the room left, and when that is too little the windows behind it are dropped
//! (fetched again later) so it gets the whole budget. The one excess left is a single chain block larger than the whole
//! budget: it is fetched alone, with nothing else held (TN10 flood blocks are about 2 MB). `prefetch_bytes_peak` reports
//! the largest sum seen; `prefetch_oversize_total` counts the refused answers.

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
    /// The size an earlier request of exactly this window reported when it was refused as too large.
    need: u64,
    /// A single chain block larger than the whole budget: sent without a size limit, alone.
    unbounded: bool,
    state: State,
}

impl Slot {
    fn new(id: u64, start: Hash32, blocks: Vec<Hash32>, timeout_scale: u32) -> Slot {
        Slot { id, start, blocks, timeout_scale, allowance: 0, need: 0, unbounded: false, state: State::Queued }
    }

    /// Bytes the window holds: its answer once fetched, its allowance in flight, nothing while queued.
    fn bytes(&self) -> u64 {
        match &self.state {
            State::Done(Ok(f)) => f.raw.wire_bytes as u64,
            State::Done(Err(_)) | State::Queued => 0,
            State::Running(_) => self.allowance,
        }
    }
}

/// Settings of the prefetch (from `FollowerConfig`).
#[derive(Debug, Clone, Copy)]
pub struct PrefetchLimits {
    /// Windows in flight at once.
    pub parallel: usize,
    pub max_bytes: u64,
    /// A window should take about this long to fetch.
    pub target: Duration,
    pub initial_blocks: usize,
    /// `minConfirmationCount` the caller wants anyway.
    pub min_confirmations: Option<u64>,
}

/// What the head of the queue holds once it is ready.
pub enum Head {
    /// The window at the cursor, checked against the plan and cut to it.
    Ready { start: Hash32, raw: RawVspcResponse, sink_blue: u64, end_blue: Option<u64>, elapsed: Duration, origin: Origin },
    /// The response does not continue the planned chain (or the node failed it otherwise): the plan was dropped.
    Discarded(String),
    /// The window timed out and was split (or given a longer timeout).
    TimedOut { blocks: usize, timeout_scale: u32 },
}

/// Counters for `/v1/health`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PrefetchStats {
    pub windows: usize,
    pub in_flight: usize,
    pub bytes: u64,
    pub peak_bytes: u64,
    pub window_blocks: usize,
    pub discards: u64,
    pub timeouts: u64,
    /// Answers refused as larger than their allowance.
    pub oversize: u64,
}

pub struct Prefetch {
    lim: PrefetchLimits,
    slots: VecDeque<Slot>,
    /// Planned chain not yet cut into windows, continuing from `tail`.
    skeleton: VecDeque<Hash32>,
    tail: Option<Hash32>,
    /// The node had no more chain after `tail` (its sink, or a reorg): plan nothing more until the queue drains.
    exhausted: bool,
    next_id: u64,
    /// Current window size in chain blocks.
    k: usize,
    /// Wire bytes per chain block, smoothed.
    bytes_per_block: Option<f64>,
    tx: mpsc::UnboundedSender<(u64, Result<Fetched, RpcError>)>,
    rx: mpsc::UnboundedReceiver<(u64, Result<Fetched, RpcError>)>,
    pub stats: PrefetchStats,
}

impl Prefetch {
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

    /// Windows are planned or in flight.
    pub fn active(&self) -> bool {
        !self.slots.is_empty() || !self.skeleton.is_empty()
    }

    /// The chain block the next window to apply starts after.
    pub fn head_start(&self) -> Option<Hash32> {
        self.slots.front().map(|s| s.start).or(if self.skeleton.is_empty() { None } else { self.tail })
    }

    /// Plan from `cursor` along `chain` (the node's selected chain after it, as hashes).
    pub fn start(&mut self, cursor: Hash32, chain: Vec<Hash32>) {
        self.reset();
        self.tail = Some(cursor);
        self.skeleton = chain.into();
    }

    /// Drop every window (in flight ones are aborted) and the plan.
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

    /// [`Prefetch::reset`] because the plan no longer matches the chain.
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

    /// Bytes a window of `blocks` chain blocks is expected to take, with a quarter on top: blocks differ in size, and the
    /// estimate is the window's allowance (a larger answer is refused and the window split).
    fn estimate(&self, blocks: usize) -> Option<u64> {
        self.bytes_per_block.map(|b| (1.25 * b * blocks as f64).ceil() as u64)
    }

    /// Chain blocks of the next window: the adaptive size, small enough that `parallel` windows fit the budget.
    fn window_blocks(&self) -> usize {
        let mut k = self.k;
        if let Some(b) = self.bytes_per_block.filter(|b| *b > 0.0) {
            let fit = (self.lim.max_bytes as f64 / (self.lim.parallel.max(1) as f64 * 1.25 * b)).floor() as usize;
            k = k.min(fit.max(1));
        }
        k.max(1)
    }

    /// Send the queued window at `i` with `allowance` bytes reserved (its size limit; none for an unbounded one).
    fn launch<S: ChainSource>(&mut self, i: usize, allowance: u64, src: &Arc<S>) {
        let s = &mut self.slots[i];
        s.allowance = allowance;
        let max = (!s.unbounded).then(|| usize::try_from(allowance).unwrap_or(usize::MAX));
        let (id, start, end, scale) = (s.id, s.start, *s.blocks.last().expect("a window is never empty"), s.timeout_scale);
        let (src, tx, extra) = (src.clone(), self.tx.clone(), self.lim.min_confirmations);
        let task = tokio::spawn(async move {
            // one window `(start, end]` at `High` (or checked `Full` from another node), bounded to end at `end`
            let w = WindowRequest { timeout_scale: scale, min_confirmations: extra, max_bytes: max, ..WindowRequest::new(start, end) };
            let r = src.fetch_window(w).await;
            let _ = tx.send((id, r));
        });
        s.state = State::Running(task.abort_handle());
    }

    /// The allowance of the queued window at `i` if it may go now. A window behind the head goes with its estimate (at least
    /// the size it reported before) while that fits the room the budget leaves; the head always goes: with its estimate,
    /// else the room left, else (too little room, or it is known to need more) after the windows behind it are dropped,
    /// with the whole budget. A head window known to need more than the whole budget is split first (`None`), a single
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
            // one chain block larger than the whole budget: alone, without a limit (the only excess)
            self.truncate(1);
            self.slots[0].unbounded = true;
            return Some(max);
        }
        if want == 0 {
            // nothing known yet (the first window): the room left
            return (room > 0).then_some(room).or_else(|| {
                self.truncate(1);
                Some(max)
            });
        }
        if want <= room {
            return Some(want);
        }
        // the room left, unless it is less than what the head is known to need or than half its estimate
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

    /// Start queued windows and plan new ones while connections and the memory budget allow.
    async fn fill<S: ChainSource>(&mut self, src: &Arc<S>) {
        loop {
            if self.running() >= self.lim.parallel.max(1) {
                break;
            }
            if let Some(i) = self.slots.iter().position(|s| matches!(s.state, State::Queued)) {
                match self.admit(i) {
                    Some(allowance) => {
                        // (admitting the head may drop the windows behind it; it stays at 0)
                        self.launch(i, allowance, src);
                        continue;
                    }
                    // the head was split: admit its first half next; any other window waits for room
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
                    Ok(c) if c.removed_chain_block_hashes.is_empty() && !c.added_chain_block_hashes.is_empty() => {
                        self.skeleton = c.added_chain_block_hashes.into();
                    }
                    // `tail` left the selected chain: cut the plan at the fork
                    Ok(c) if !c.removed_chain_block_hashes.is_empty() => {
                        self.fork_cut(&c.removed_chain_block_hashes);
                        break;
                    }
                    // at the sink, or the node failed: plan nothing more until the queue drains
                    _ => {
                        self.exhausted = true;
                        break;
                    }
                }
            }
            let k = self.window_blocks().min(self.skeleton.len());
            if !self.slots.is_empty() {
                // the budget: the window at the cursor always goes, any other only within it (and once sizes are known)
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
            // a source that ignores the request's size limit: its answer is dropped as if the transport had refused it
            Ok(f) if !self.slots[i].unbounded && f.raw.wire_bytes as u64 > allowance => {
                Err(RpcError::TooLarge { size: f.raw.wire_bytes, limit: usize::try_from(allowance).unwrap_or(usize::MAX) })
            }
            r => r,
        };
        // learn the size of chain blocks from every answer, at once when they grew (a refused one reports its size)
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

    /// Split the window at `i` (in place) after a timeout or an answer too large for it: two halves, or for a single chain
    /// block a longer timeout (a timeout) or an allowance of the size it reported (too large).
    fn split(&mut self, i: usize) -> (usize, u32) {
        let s = self.slots.remove(i).expect("index");
        let too_large = match &s.state {
            State::Done(Err(RpcError::TooLarge { size, .. })) => Some(*size as u64),
            _ => None,
        };
        match (&s.state, too_large) {
            (_, Some(_)) => self.stats.oversize += 1,
            // a queued head known to need more than the whole budget (`admit`)
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

    /// Drop the window at `i` and every later one, and plan nothing more until the queue drains.
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

    /// The node reported `removed` chain blocks (a reorg): every window that holds or starts after one of them lies
    /// past the fork and is dropped, fetched or not; the windows before the fork stay.
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

    /// Windows that came back: one too large for its allowance (the head too) is split at once; behind the head, a reorg
    /// one cuts the plan at the fork, a timed-out one is split at once, any other error drops that window and every later
    /// one (the plan past it is unproven).
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.slots.len() {
            match &self.slots[i].state {
                State::Done(Err(RpcError::TooLarge { .. })) => {
                    self.split(i);
                    i += 1;
                }
                // the head's other outcomes are `next`'s
                _ if i == 0 => i += 1,
                State::Done(Ok(f)) if !f.raw.removed_chain_block_hashes.is_empty() => {
                    let removed = f.raw.removed_chain_block_hashes.clone();
                    self.fork_cut(&removed);
                    // the head may be past the fork too: the caller sees the queue as it is now
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

    /// Wait for the window at the head, keeping the others going meanwhile. Call only while [`Prefetch::active`].
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
        // learn the size and speed of a window (sizes rise at once on arrival, `on_message`; here they settle down again)
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
            // cut short by the node (merged-block limit): fetch the rest next
            self.push_window(true, planned[n - 1], planned[n..].to_vec(), 1);
        }
        self.refresh_stats();
        let end_blue = (n == planned.len()).then_some(end_blue);
        Head::Ready { start: head.start, raw, sink_blue, end_blue, elapsed, origin }
    }
}
