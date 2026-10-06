//! Intermittent mining: mine in bursts while the watched balance is low, idle otherwise, never above a duty cycle.
//!
//! Hysteresis on the spendable balance: a burst starts when it falls below `low` and ends when it reaches `high` (between the two the
//! current state holds). Independently, the fraction of time spent mining over the last `window` never exceeds `max_duty`. Without
//! watermarks the miner mines continuously (subject to the duty cap), as before.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub struct ThrottleConfig {
    /// (low, high) in sompi: start a burst below `low`, stop at or above `high`
    pub watermarks: Option<(u64, u64)>,
    /// 0 < max_duty <= 1
    pub max_duty: f64,
    pub window: Duration,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self { watermarks: None, max_duty: 1.0, window: Duration::from_secs(600) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// no watermarks: always mine
    Continuous,
    /// a burst: the balance fell below `low` and has not reached `high` yet
    Burst,
    /// the balance is at or above `high`, or has not fallen below `low` since
    BalanceOk,
    /// the duty-cycle cap is reached
    DutyCap,
    /// watermarks set but no balance read yet
    Unknown,
}

pub struct Throttle {
    pub cfg: ThrottleConfig,
    /// hysteresis state of the balance rule (true = in a burst)
    burst: bool,
    balance_known: bool,
    /// closed mining intervals inside the window, oldest first
    done: VecDeque<(Instant, Instant)>,
    /// start of the current mining interval
    open: Option<Instant>,
}

impl Throttle {
    pub fn new(cfg: ThrottleConfig) -> Self {
        Self { cfg, burst: false, balance_known: false, done: VecDeque::new(), open: None }
    }

    /// Feed a fresh balance reading (sompi). Returns Some(true/false) when the burst state changed.
    pub fn observe_balance(&mut self, spendable: u64) -> Option<bool> {
        self.balance_known = true;
        let (low, high) = self.cfg.watermarks?;
        let before = self.burst;
        if spendable < low {
            self.burst = true;
        } else if spendable >= high {
            self.burst = false;
        }
        (before != self.burst).then_some(self.burst)
    }

    /// Mining time inside the window ending at `now`.
    pub fn mined_in_window(&self, now: Instant) -> Duration {
        // (an Instant earlier than the window may not exist shortly after boot: then everything counts)
        let from = now.checked_sub(self.cfg.window);
        let clip = |(a, b): (Instant, Instant)| b.saturating_duration_since(from.map_or(a, |f| a.max(f)));
        let closed: Duration = self.done.iter().map(|&iv| clip(iv)).sum();
        closed + self.open.map(|a| clip((a, now))).unwrap_or_default()
    }

    pub fn duty(&self, now: Instant) -> f64 {
        self.mined_in_window(now).as_secs_f64() / self.cfg.window.as_secs_f64()
    }

    /// Whether to mine now, and why.
    pub fn decide(&self, now: Instant) -> (bool, Reason) {
        let balance = match self.cfg.watermarks {
            None => (true, Reason::Continuous),
            Some(_) if !self.balance_known => (false, Reason::Unknown),
            Some(_) if self.burst => (true, Reason::Burst),
            Some(_) => (false, Reason::BalanceOk),
        };
        if balance.0
            && let Some(budget) = self.duty_budget(now)
        {
            // a paused miner resumes only with a useful budget left (not one template per few ms at the cap)
            let need = if self.is_mining() { Duration::ZERO } else { self.resume_min() };
            if budget.is_zero() || budget < need {
                return (false, Reason::DutyCap);
            }
        }
        balance
    }

    /// Record whether the miner is mining from `now` on.
    pub fn set_mining(&mut self, now: Instant, mining: bool) {
        match (self.open, mining) {
            (None, true) => self.open = Some(now),
            (Some(a), false) => {
                self.done.push_back((a, now));
                self.open = None;
            }
            _ => {}
        }
        let Some(from) = now.checked_sub(self.cfg.window) else { return };
        while self.done.front().is_some_and(|&(_, b)| b <= from) {
            self.done.pop_front();
        }
    }

    pub fn is_mining(&self) -> bool {
        self.open.is_some()
    }

    /// The duty budget a paused miner needs before it resumes: min(5 s, a quarter of the allowed mining time per window).
    pub fn resume_min(&self) -> Duration {
        self.cfg.window.mul_f64(self.cfg.max_duty / 4.0).min(Duration::from_secs(5))
    }

    /// How long mining may continue from `now` before the duty cap is hit (None = no cap).
    pub fn duty_budget(&self, now: Instant) -> Option<Duration> {
        if self.cfg.max_duty >= 1.0 {
            return None;
        }
        let allowed = self.cfg.window.mul_f64(self.cfg.max_duty);
        Some(allowed.saturating_sub(self.mined_in_window(now)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KAS: u64 = 100_000_000;

    fn wm(low: u64, high: u64) -> ThrottleConfig {
        ThrottleConfig { watermarks: Some((low * KAS, high * KAS)), ..Default::default() }
    }

    #[test]
    fn continuous_without_watermarks() {
        let t = Throttle::new(ThrottleConfig::default());
        assert_eq!(t.decide(Instant::now()), (true, Reason::Continuous));
    }

    #[test]
    fn idles_until_a_balance_is_known() {
        let t = Throttle::new(wm(100, 200));
        assert_eq!(t.decide(Instant::now()), (false, Reason::Unknown));
    }

    #[test]
    fn hysteresis_between_watermarks() {
        let now = Instant::now();
        let mut t = Throttle::new(wm(100, 200));
        assert_eq!(t.observe_balance(150 * KAS), None); // between: stays idle
        assert_eq!(t.decide(now), (false, Reason::BalanceOk));
        assert_eq!(t.observe_balance(99 * KAS), Some(true)); // below low: burst
        assert_eq!(t.decide(now), (true, Reason::Burst));
        assert_eq!(t.observe_balance(150 * KAS), None); // rising but below high: keep mining
        assert_eq!(t.decide(now), (true, Reason::Burst));
        assert_eq!(t.observe_balance(200 * KAS), Some(false)); // at high: stop
        assert_eq!(t.decide(now), (false, Reason::BalanceOk));
        assert_eq!(t.observe_balance(120 * KAS), None); // falling but above low: stay idle
        assert_eq!(t.decide(now), (false, Reason::BalanceOk));
        assert_eq!(t.observe_balance(100 * KAS), None); // exactly low is not below it
        assert_eq!(t.observe_balance(100 * KAS - 1), Some(true));
    }

    #[test]
    fn duty_cap_over_the_window() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut t = Throttle::new(ThrottleConfig { watermarks: None, max_duty: 0.25, window: s(100) });
        t.set_mining(t0, true);
        assert_eq!(t.duty_budget(t0), Some(s(25)));
        assert_eq!(t.decide(t0 + s(24)), (true, Reason::Continuous));
        assert_eq!(t.decide(t0 + s(25)), (false, Reason::DutyCap));
        t.set_mining(t0 + s(25), false);
        // the 25 s burst stays inside the window until t0+125: capped until the oldest mining slides out
        assert_eq!(t.decide(t0 + s(60)), (false, Reason::DutyCap));
        // at t0+104 the window [4, 104] holds 21 s: a 4 s budget is below the 5 s needed to resume
        assert_eq!(t.duty_budget(t0 + s(104)), Some(s(4)));
        assert_eq!(t.decide(t0 + s(104)), (false, Reason::DutyCap));
        assert!((t.duty(t0 + s(110)) - 0.15).abs() < 1e-9); // [10, 25) still inside
        assert_eq!(t.decide(t0 + s(110)), (true, Reason::Continuous));
        assert_eq!(t.duty_budget(t0 + s(110)), Some(s(10)));
        t.set_mining(t0 + s(200), true);
        assert_eq!(t.duty(t0 + s(200)), 0.0);
        assert!(t.done.is_empty(), "old intervals are pruned");
    }

    #[test]
    fn duty_cap_applies_inside_a_burst() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut t = Throttle::new(ThrottleConfig { max_duty: 0.5, window: s(10), ..wm(100, 200) });
        t.observe_balance(10 * KAS);
        t.set_mining(t0, true);
        assert_eq!(t.decide(t0 + s(4)), (true, Reason::Burst));
        assert_eq!(t.decide(t0 + s(5)), (false, Reason::DutyCap));
    }
}
