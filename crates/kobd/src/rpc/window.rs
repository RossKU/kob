//! Bounding `getVirtualChainFromBlockV2` responses.
//!
//! The node returns the selected chain from the start hash up to its sink, capped only by chain blocks
//! (`mergeset_size_limit * 10`) and merged blocks, never by bytes: a follower that falls behind under a
//! transaction flood asks for an ever larger batch (198k transactions, ~300 MB on TN10 on 10-01) until no
//! response arrives within the request timeout, and retrying the same request then times out forever.
//!
//! The request has no size parameter, but `minConfirmationCount` strips the chain head: the node keeps an
//! added chain block only while `sink_blue_score - block_blue_score > minConfirmationCount`. With
//! `minConfirmationCount = sink_blue - (cursor_blue + window)` the response covers the chain blocks whose
//! blue score is in `(cursor_blue, cursor_blue + window)`: a window of blue score, about one block each at
//! 10 BPS. [`BatchWindow`] keeps that window adaptive: it shrinks after a timeout or a slow batch, grows
//! after a fast one, grows when a bounded request came back empty (the next chain block lies beyond the
//! window), and when it cannot shrink any further it lengthens the request timeout instead, so the same
//! failing request is never sent forever.

use std::time::Duration;

/// Adaptive size of the next chain batch, in blue score.
#[derive(Debug, Clone)]
pub struct BatchWindow {
    cur: u64,
    min: u64,
    max: u64,
    /// From the current cursor: the largest window that came back empty (0: none) and the smallest one
    /// that timed out (`u64::MAX`: none). The next chain block lies between them; once they are adjacent
    /// the smallest batch that can hold it still times out, and only a longer timeout helps.
    empty_at: u64,
    timeout_at: u64,
    /// A batch should take about this long to fetch.
    target: Duration,
    /// Multiplier of the client's request timeout (1 until the window cannot shrink and still times out).
    timeout_scale: u32,
    max_timeout_scale: u32,
}

/// What a request should ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowPlan {
    /// `minConfirmationCount` of the request (`None`: ask for everything up to the sink).
    pub min_confirmations: Option<u64>,
    /// The blue score the response is bounded by (exclusive) when the window, not the sink, ends it.
    pub until_blue: Option<u64>,
    /// Blue score the request covers: the window, or the distance to the sink when that is shorter.
    pub span: u64,
    pub timeout_scale: u32,
}

impl WindowPlan {
    /// Bounded by the window (as opposed to by the caller's confirmation depth or nothing): the node has more chain.
    pub fn by_window(&self) -> bool {
        self.until_blue.is_some()
    }
}

impl BatchWindow {
    pub fn new(initial: u64, min: u64, max: u64, target: Duration) -> Self {
        let min = min.max(1);
        let max = max.max(min);
        BatchWindow {
            cur: initial.clamp(min, max),
            min,
            max,
            empty_at: 0,
            timeout_at: u64::MAX,
            target,
            timeout_scale: 1,
            max_timeout_scale: 8,
        }
    }

    /// Defaults for a client whose requests time out after `timeout`: start at 600 blue score (about a
    /// minute of chain at 10 BPS) and aim for a sixth of the timeout per batch.
    pub fn for_timeout(timeout: Duration) -> Self {
        BatchWindow::new(600, 1, 1 << 20, (timeout / 6).max(Duration::from_secs(1)))
    }

    pub fn current(&self) -> u64 {
        self.cur
    }

    pub fn timeout_scale(&self) -> u32 {
        self.timeout_scale
    }

    /// The request for a cursor at `cursor_blue` while the node's sink is at `sink_blue`. `extra` is a
    /// `minConfirmationCount` the caller wants anyway (the larger one wins).
    pub fn plan(&self, cursor_blue: u64, sink_blue: u64, extra: Option<u64>) -> WindowPlan {
        let until = cursor_blue.saturating_add(self.cur);
        let timeout_scale = self.timeout_scale;
        if sink_blue > until {
            let conf = (sink_blue - until).max(extra.unwrap_or(0));
            WindowPlan { min_confirmations: Some(conf), until_blue: Some(until), span: self.cur, timeout_scale }
        } else {
            WindowPlan { min_confirmations: extra, until_blue: None, span: sink_blue.saturating_sub(cursor_blue), timeout_scale }
        }
    }

    /// The cursor moved: what was learnt about the previous one no longer applies.
    pub fn cursor_moved(&mut self) {
        self.timeout_scale = 1;
        self.empty_at = 0;
        self.timeout_at = u64::MAX;
    }

    /// A batch arrived in `fetch` and the cursor moved. `by_window`: the window (not the sink) ended it.
    pub fn on_success(&mut self, fetch: Duration, by_window: bool) {
        self.cursor_moved();
        if fetch > self.target {
            self.cur = (self.cur / 2).max(self.min);
        } else if by_window && fetch < self.target / 2 {
            self.cur = self.cur.saturating_mul(2).min(self.max);
        }
    }

    /// A request bounded by the window came back without a chain block: the next one lies beyond it.
    /// Grow, but not into a window that already timed out (bisect between the two instead).
    pub fn on_empty(&mut self) {
        self.empty_at = self.empty_at.max(self.cur);
        let next = if self.timeout_at == u64::MAX {
            self.cur.saturating_mul(2).min(self.max)
        } else {
            self.empty_at + (self.timeout_at - self.empty_at) / 2
        };
        if next > self.empty_at {
            self.cur = next;
        } else {
            // the smallest window that can hold the next block timed out before: give it more time
            self.cur = self.timeout_at;
            self.escalate();
        }
    }

    /// A request covering `span` blue score timed out (`span` is below the window when the sink ended the
    /// request): ask for less (a quarter, or half way down to a window that came back empty), or, when the
    /// window cannot shrink any further, wait longer for the same window.
    pub fn on_timeout(&mut self, span: u64) {
        self.cur = self.cur.min(span.max(self.min));
        if self.empty_at >= self.cur {
            // learnt before the chain changed under the cursor: no longer a bound
            self.empty_at = 0;
        }
        self.timeout_at = self.timeout_at.min(self.cur);
        let next = if self.empty_at == 0 { self.cur / 4 } else { self.empty_at + (self.cur - self.empty_at.min(self.cur)) / 2 };
        let next = next.max(self.empty_at + 1).max(self.min);
        if next < self.cur {
            self.cur = next;
        } else {
            self.escalate();
        }
    }

    fn escalate(&mut self) {
        self.timeout_scale = (self.timeout_scale * 2).min(self.max_timeout_scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_bounds_only_when_the_window_ends_before_the_sink() {
        let w = BatchWindow::new(100, 1, 10_000, Duration::from_secs(30));
        // far behind: minConfirmationCount keeps blue scores below cursor + 100
        let p = w.plan(1_000, 5_000, None);
        assert_eq!(p.min_confirmations, Some(3_900));
        assert_eq!(p.until_blue, Some(1_100));
        assert!(p.by_window());
        // the node keeps a block while sink - blue > minConf, i.e. blue < 1_100
        let keeps = |blue: u64| 5_000 - blue > p.min_confirmations.unwrap();
        assert!(keeps(1_099) && !keeps(1_100));
        // close to the sink: ask for everything
        let p = w.plan(4_950, 5_000, None);
        assert_eq!(p.min_confirmations, None);
        assert!(!p.by_window());
        // a caller depth applies either way, and the larger of the two wins
        assert_eq!(w.plan(4_950, 5_000, Some(10)).min_confirmations, Some(10));
        assert!(!w.plan(4_950, 5_000, Some(10)).by_window());
        assert_eq!(w.plan(1_000, 5_000, Some(10)).min_confirmations, Some(3_900));
        assert_eq!(w.plan(1_000, 5_000, Some(4_000)).min_confirmations, Some(4_000));
        // a sink behind the cursor (another node, a reorg): no window
        assert_eq!(w.plan(6_000, 5_000, None).min_confirmations, None);
    }

    #[test]
    fn the_window_adapts_to_the_fetch_time() {
        let mut w = BatchWindow::new(1_000, 1, 1 << 20, Duration::from_secs(30));
        w.on_timeout(u64::MAX);
        assert_eq!(w.current(), 250);
        w.on_success(Duration::from_secs(40), true); // slow: halve
        assert_eq!(w.current(), 125);
        w.on_success(Duration::from_secs(20), true); // in band: keep
        assert_eq!(w.current(), 125);
        w.on_success(Duration::from_secs(5), true); // fast: double
        assert_eq!(w.current(), 250);
        w.on_success(Duration::from_secs(5), false); // fast but the sink ended it: nothing learned
        assert_eq!(w.current(), 250);
        w.on_empty();
        assert_eq!(w.current(), 500);
        // a request the sink ended (60 blue behind) timed out: the next one is bounded, to a quarter of it
        w.cursor_moved();
        let p = w.plan(1_000, 1_060, None);
        assert!(!p.by_window() && p.span == 60);
        w.on_timeout(p.span);
        assert_eq!(w.current(), 15);
        assert_eq!(w.plan(1_000, 1_060, None).until_blue, Some(1_015));
    }

    /// Replays a cursor whose next chain block lies `gap` blue score ahead and whose smallest batch
    /// takes `need` timeouts' worth of time: the requests must change until one succeeds.
    fn converge(gap: u64, need: u32) -> Vec<(u64, u32)> {
        let mut w = BatchWindow::new(1_000, 1, 1 << 20, Duration::from_secs(30));
        let mut seen = vec![];
        for _ in 0..64 {
            let req = (w.current(), w.timeout_scale());
            seen.push(req);
            if w.current() <= gap {
                w.on_empty();
            } else if w.timeout_scale() < need {
                w.on_timeout(u64::MAX);
            } else {
                w.on_success(Duration::from_secs(1), true);
                return seen;
            }
        }
        panic!("no progress: {seen:?}");
    }

    #[test]
    fn a_timed_out_request_is_never_repeated_unchanged() {
        for gap in [0, 1, 3, 40, 251, 999, 5_000] {
            for need in [1, 2, 4, 8] {
                let seen = converge(gap, need);
                assert!(seen.windows(2).all(|p| p[0] != p[1]), "gap {gap} need {need}: {seen:?}");
            }
        }
        // the smallest window that can hold the next chain block, then a longer timeout
        let seen = converge(1, 8);
        assert_eq!(seen.last(), Some(&(2, 8)), "{seen:?}");
    }
}
