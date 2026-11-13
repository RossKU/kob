//! Judging a node by its answers, and not only by its connection.
//!
//! A node can accept the connection and still be useless. The checks below treat each of these as a
//! failure of the node, like a lost connection. They apply to every node of a pair, to the chain
//! authority and to the cross-checking node alike.
//!
//! - It is on another network.
//! - It has no UTXO index, when the process needs one (the Executor reads its funds there).
//! - It is not synced.
//! - It hangs, so its requests time out.
//! - It is behind the other node of the pair (the health watch of `rpc::multi`).
//! - It contradicts the other node.
//! - It is the chain authority and its chain requests keep failing while the cursor stalls
//!   ([`chain_failing`]).
//!
//! # Public and named nodes
//!
//! A resolver entry (`rpc::resolver`) stands for a public node. The first finding gives the node up
//! for another one at once ([`resolver::replace`]). The reason is logged as a warning and shown per
//! node as `cut_reason` in `/v1/health`. The findings that count are:
//!
//! - another network;
//! - no UTXO index, when [`require_utxo_index`] was called;
//! - not synced for [`UNSYNCED_GRACE`];
//! - [`TIMEOUTS`] request timeouts within [`TIMEOUT_WINDOW`];
//! - behind the other node for a minute, as the node itself confirmed (a node's claim to be ahead
//!   never counts);
//! - a contradiction of the other node;
//! - chain requests that keep failing while the cursor stalls, for the chain authority.
//!
//! A node the operator named with a fixed URL is never replaced. The same findings raise an alarm.
//! The alarm is logged as an error when it begins. It is listed under `node_alarms` in
//! `/v1/health`, shown as the node's `cut_reason`, counted by `kob_indexer_node_alarms` and added
//! to the status line. It lasts until the node is fine again.
//!
//! A lag, sync or chain alarm ends only after [`ALARM_CLEAR_AFTER`] without a break. A node that
//! hovers at the bound therefore does not flap.
//!
//! # Effect of an alarm
//!
//! - A cross-checking node with an alarm is left out of the pair meanwhile. The authority carries
//!   on alone.
//! - A cross-checking node that contradicted the authority is dropped by that pair until a restart.
//! - The authority keeps its role, except when its chain requests keep failing. Then another node
//!   of the list that can lead takes the role over (`rpc::multi`).
//!
//! # What is not counted
//!
//! A VSPC request that times out is not counted, because the batch windows adapt to a slow link
//! (`rpc::window`). The small calls (`getServerInfo`, `getBlock`, `getSinkBlueScore` and the like)
//! are what a hanging node fails.

use super::{redact_url, resolver};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A node that reports not synced for this long is given up (public) or raises an alarm (named). A
/// public node reports not synced for a few seconds now and then. A node that has fallen out of the
/// network for minutes does not come back soon.
pub const UNSYNCED_GRACE: Duration = Duration::from_secs(120);

/// Request timeouts of one node within [`TIMEOUT_WINDOW`] that make it a hanging node.
pub const TIMEOUTS: usize = 3;
/// The window in which [`TIMEOUTS`] timeouts make a node a hanging node.
pub const TIMEOUT_WINDOW: Duration = Duration::from_secs(600);

/// A named node's lag or sync alarm ends once the node has been fine for this long without a break.
/// One fine answer is not enough, because a node hovering around the bound would raise an error
/// every minute or two.
pub const ALARM_CLEAR_AFTER: Duration = Duration::from_secs(60);

/// [`ALARM_CLEAR_AFTER`] in milliseconds, or the value a test set ([`set_alarm_clear_after`]).
static CLEAR_AFTER_MS: AtomicU64 = AtomicU64::new(ALARM_CLEAR_AFTER.as_millis() as u64);

/// How long a lag or sync alarm needs fine answers to end. A test sets zero to end it at the first
/// one.
#[doc(hidden)]
pub fn set_alarm_clear_after(d: Duration) {
    CLEAR_AFTER_MS.store(d.as_millis() as u64, Ordering::Relaxed);
}

fn clear_after() -> Duration {
    Duration::from_millis(CLEAR_AFTER_MS.load(Ordering::Relaxed))
}

/// The alarms that end only after [`ALARM_CLEAR_AFTER`] of fine answers. The others end with their
/// finding.
fn sticky(kind: Kind) -> bool {
    matches!(kind, Kind::Lag | Kind::Sync | Kind::Chain)
}

/// What a finding is about: one alarm per named node and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The node is on another network.
    Network,
    /// The node has no UTXO index, which the process needs.
    UtxoIndex,
    /// The node reports not synced.
    Sync,
    /// The node hangs, so its requests time out.
    Timeouts,
    /// The node is behind the other node, or its sink stands still.
    Lag,
    /// The node contradicted the other node.
    Contradiction,
    /// Its chain requests keep failing while the cursor does not move (`rpc::multi`,
    /// `MultiNode::authority_failing`).
    Chain,
}

#[derive(Default)]
struct Judge {
    /// Node to the time since it reports not synced.
    unsynced: HashMap<String, Instant>,
    /// Node to its recent request timeouts, oldest first.
    timeouts: HashMap<String, VecDeque<Instant>>,
    /// Nodes that were behind the other node of their pair at the last health check.
    behind: HashSet<String>,
    /// (Named node, kind) to what is wrong.
    alarms: BTreeMap<(String, Kind), String>,
    /// (Named node, kind) of a sticky alarm to the time since its node has been fine without a
    /// break.
    fine_since: BTreeMap<(String, Kind), Instant>,
}

impl Judge {
    /// A named node's findings now. Returns the alarms that begin (what is wrong) and the kinds
    /// that end. A sticky alarm ([`sticky`]) ends only after [`clear_after`] of fine answers in a
    /// row.
    fn alarm(&mut self, url: &str, findings: &[(Kind, Option<String>)], now: Instant) -> (Vec<String>, Vec<Kind>) {
        let (mut began, mut ended) = (vec![], vec![]);
        for (kind, why) in findings {
            let key = (url.to_string(), *kind);
            match why {
                Some(w) => {
                    self.fine_since.remove(&key);
                    if self.alarms.insert(key, w.clone()).is_none() {
                        began.push(w.clone());
                    }
                }
                None if !self.alarms.contains_key(&key) => {}
                None => {
                    let since = *self.fine_since.entry(key.clone()).or_insert(now);
                    if !sticky(*kind) || now.saturating_duration_since(since) >= clear_after() {
                        self.fine_since.remove(&key);
                        self.alarms.remove(&key);
                        ended.push(*kind);
                    }
                }
            }
        }
        (began, ended)
    }

    /// Why `node` counts as not synced now. `None` means synced, or not for long enough yet.
    fn sync(&mut self, node: &str, synced: bool, now: Instant) -> Option<String> {
        if synced {
            self.unsynced.remove(node);
            return None;
        }
        let since = *self.unsynced.entry(node.to_string()).or_insert(now);
        (now.saturating_duration_since(since) >= UNSYNCED_GRACE)
            .then(|| format!("it has reported not synced for over {} min", UNSYNCED_GRACE.as_secs() / 60))
    }

    /// A request to `node` timed out. Returns why it counts as hanging, or `None` when not often
    /// enough yet.
    fn timed_out(&mut self, node: &str, now: Instant) -> Option<String> {
        let q = self.timeouts.entry(node.to_string()).or_default();
        q.push_back(now);
        prune(q, now);
        (q.len() >= TIMEOUTS).then(|| format!("{TIMEOUTS} requests timed out within {} min (it hangs)", TIMEOUT_WINDOW.as_secs() / 60))
    }

    /// Forgets everything known about a node that was given up, so that its successor starts clean.
    fn forget(&mut self, node: &str) {
        self.unsynced.remove(node);
        self.timeouts.remove(node);
        self.behind.remove(node);
    }

    /// Ends the timeout alarms whose timeouts left the window.
    fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut ended = vec![];
        let timeouts = &mut self.timeouts;
        self.alarms.retain(|(url, kind), _| {
            if *kind != Kind::Timeouts {
                return true;
            }
            let live = timeouts.get_mut(url).map(|q| {
                prune(q, now);
                q.len()
            });
            let keep = live.unwrap_or(0) >= TIMEOUTS;
            if !keep {
                ended.push(url.clone());
            }
            keep
        });
        ended
    }
}

fn prune(q: &mut VecDeque<Instant>, now: Instant) {
    while q.front().is_some_and(|t| now.saturating_duration_since(*t) > TIMEOUT_WINDOW) {
        q.pop_front();
    }
}

fn lock() -> std::sync::MutexGuard<'static, Judge> {
    static J: OnceLock<Mutex<Judge>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(Judge::default())).lock().unwrap_or_else(|e| e.into_inner())
}

static NEED_UTXO_INDEX: AtomicBool = AtomicBool::new(false);

/// The process needs the UTXO index of its nodes (`kobd --executor` reads the operator's funds
/// there). A node without it is a finding from now on.
pub fn require_utxo_index() {
    NEED_UTXO_INDEX.store(true, Ordering::Relaxed);
}

/// Whether the process needs the UTXO index of its nodes ([`require_utxo_index`]).
pub fn utxo_index_required() -> bool {
    NEED_UTXO_INDEX.load(Ordering::Relaxed)
}

/// Acts on the findings about `node`, the node the configured `url` stands for. The first finding
/// gives a resolver entry's node up, and the node is returned. For a named node the alarms begin
/// and end with the findings. It is never called with the judge's lock held, because
/// [`resolver::replace`] asks [`suspect`].
fn act(url: &str, node: &str, findings: &[(Kind, Option<String>)]) -> Option<(Kind, String)> {
    if resolver::is_resolver(url) {
        let bad = findings.iter().find_map(|(k, w)| w.clone().map(|w| (*k, w)));
        if let Some((_, why)) = &bad {
            // A report about a node the entry no longer uses changes nothing. Another client
            // reported it first.
            if resolver::current(url).as_deref() == Some(node) {
                lock().forget(node);
                resolver::replace(url, node, why);
            }
        }
        return bad;
    }
    let (began, ended) = lock().alarm(url, findings, Instant::now());
    for w in began {
        tracing::error!(
            node = %redact_url(url),
            problem = %w,
            "NODE ALARM: a node named in the configuration is not usable (it is never replaced: fix it, or use the public nodes, rpc_url = \"resolver\")"
        );
    }
    for k in ended {
        tracing::info!(node = %redact_url(url), what = ?k, "node alarm cleared");
    }
    None
}

/// Takes a `getServerInfo` answer of `node`, the node the configured `url` stands for now.
///
/// `Err` means the answer must not be used. The public node is on another network and was given up,
/// so the caller retries on its successor. A named node on another network raises an alarm and its
/// answer is used as it is, because the follower and the runner refuse another network themselves.
pub fn server_info(url: &str, node: &str, network_id: &str, synced: bool, utxo_index: bool) -> Result<(), String> {
    let network = resolver::network();
    let need_utxo = NEED_UTXO_INDEX.load(Ordering::Relaxed);
    let sync = lock().sync(node, synced, Instant::now());
    let findings = [
        (Kind::Network, (!network.is_empty() && network_id != network).then(|| format!("it is on {network_id}, not {network}"))),
        (Kind::UtxoIndex, (need_utxo && !utxo_index).then(|| "it has no UTXO index (the Executor reads its funds there)".to_string())),
        (Kind::Sync, sync),
    ];
    match act(url, node, &findings) {
        Some((Kind::Network, why)) => Err(format!("public node {}: {why}; given up for another", resolver::host(node))),
        _ => Ok(()),
    }
}

/// A small request to `node` timed out. A VSPC batch is not counted, because those adapt
/// (`rpc::window`).
pub fn timed_out(url: &str, node: &str) {
    let why = lock().timed_out(node, Instant::now());
    if why.is_some() {
        act(url, node, &[(Kind::Timeouts, why)]);
    }
}

/// The pair's health watch (`rpc::multi`) compared `node` with the other node.
///
/// `behind` is the lag in DAA when the answers put the node behind by more than the watch's bound.
/// The node is not promoted to authority meanwhile ([`suspect`]). `confirmed` is set once the node
/// itself confirmed that lag over the watch's whole period. That is a finding, because the sink of
/// the other node, which it learned later, was that far ahead. `None` for both means level, or the
/// other node's lead was not borne out, which is a fine answer.
pub fn lag(url: &str, node: &str, behind: Option<u64>, confirmed: Option<u64>) {
    {
        let mut j = lock();
        if behind.is_some() || confirmed.is_some() {
            j.behind.insert(node.to_string());
        } else {
            j.behind.remove(node);
        }
    }
    match (behind, confirmed) {
        (_, Some(d)) => {
            act(url, node, &[(Kind::Lag, Some(format!("it stays {d} DAA behind the other node (about {} s of chain)", d / 10)))]);
        }
        (None, None) => {
            act(url, node, &[(Kind::Lag, None)]);
        }
        (Some(_), None) => {}
    }
}

/// `node` is behind by the other node's answers only, with no evidence of its own yet
/// (`rpc::multi`). It is not suspect, and its alarms neither begin nor end.
pub fn unconfirmed(node: &str) {
    lock().behind.remove(node);
}

/// The health watch (`rpc::multi`) found the sink of `node` standing still, as `why` says, on the
/// local monotonic clock. Either the other node's sink advances, or no other node exists and the
/// sink stood still for long. The node stopped advancing, though it may still report synced. This
/// is a finding like a confirmed lag.
pub fn frozen(url: &str, node: &str, why: &str) {
    lock().behind.insert(node.to_string());
    act(url, node, &[(Kind::Lag, Some(why.to_string()))]);
}

/// `node` contradicted the other node of the pair (`rpc::multi`: a hash or the acceptance data).
/// The alarm of a named node stays until the process restarts. The pair drops that node for good.
pub fn contradicted(url: &str, node: &str, why: &str) {
    act(url, node, &[(Kind::Contradiction, Some(format!("it contradicted the other node: {why}")))]);
}

/// The chain requests of `node` keep failing, as `why` says, while the follower's cursor does not
/// move. The callers are `MultiNode::authority_failing` (`rpc::multi`) and the probe of a demoted
/// node (`multi::probe_demoted`).
///
/// A public node is given up for another. The other node of the pair is the authority at once,
/// unless it is suspect. A named node raises a sticky alarm. It ends after [`ALARM_CLEAR_AFTER`] of
/// chain answers ([`chain_ok`]).
pub fn chain_failing(url: &str, node: &str, why: &str) {
    act(url, node, &[(Kind::Chain, Some(why.to_string()))]);
}

/// `node` answered a chain request. That is a fine answer for a chain alarm ([`chain_failing`]).
pub fn chain_ok(url: &str, node: &str) {
    act(url, node, &[(Kind::Chain, None)]);
}

/// Whether the named node `url` has a chain alarm now ([`chain_failing`]).
pub fn chain_alarm(url: &str) -> bool {
    lock().alarms.contains_key(&(url.to_string(), Kind::Chain))
}

/// Whether the judge suspects `node` now. It reports not synced, or it was behind the other node at
/// the last check. Such a node is not promoted to chain authority (`rpc::resolver`).
pub fn suspect(node: &str) -> bool {
    let j = lock();
    j.unsynced.contains_key(node) || j.behind.contains(node)
}

fn expire() {
    let ended = lock().expire(Instant::now());
    for url in ended {
        tracing::info!(node = %redact_url(&url), what = ?Kind::Timeouts, "node alarm cleared");
    }
}

/// Whether the named node `url` has an alarm that cuts it off from a pair now (`rpc::multi`).
///
/// A contradiction does not cut off. The pair that saw it drops the node itself until the process
/// restarts. Another pair over the same URL judges the node on what it sees.
pub fn cuts_off(url: &str) -> bool {
    expire();
    lock().alarms.keys().any(|(u, k)| u == url && *k != Kind::Contradiction)
}

/// The alarms of the named node `url`, joined. `None` means none. `/v1/health` shows them as its
/// `cut_reason`.
pub fn alarm_of(url: &str) -> Option<String> {
    expire();
    let j = lock();
    let v: Vec<&str> = j.alarms.iter().filter(|((u, _), _)| u == url).map(|(_, w)| w.as_str()).collect();
    (!v.is_empty()).then(|| v.join("; "))
}

/// Every alarm of a named node now as `node: problem`, with the URL redacted. `/v1/health` shows
/// them as `node_alarms`.
pub fn alarms() -> Vec<String> {
    expire();
    lock().alarms.iter().map(|((url, _), w)| format!("{}: {w}", redact_url(url))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_synced_counts_after_the_grace_and_timeouts_within_the_window() {
        let mut j = Judge::default();
        let t0 = Instant::now();
        assert_eq!(j.sync("n", false, t0), None, "a moment of not synced is no finding");
        assert_eq!(j.sync("n", false, t0 + UNSYNCED_GRACE / 2), None);
        assert!(j.sync("n", false, t0 + UNSYNCED_GRACE).unwrap().contains("not synced"));
        assert_eq!(j.sync("n", true, t0 + UNSYNCED_GRACE), None);
        assert_eq!(j.sync("n", false, t0 + UNSYNCED_GRACE * 2), None, "synced in between: the grace begins again");
        // timeouts: three within the window
        assert_eq!(j.timed_out("n", t0), None);
        assert_eq!(j.timed_out("n", t0 + Duration::from_secs(60)), None);
        assert!(j.timed_out("n", t0 + Duration::from_secs(120)).unwrap().contains("hangs"));
        // spread wider than the window: never three in it
        let mut k = Judge::default();
        for i in 0..6u64 {
            assert_eq!(k.timed_out("m", t0 + TIMEOUT_WINDOW * i as u32 / 2 + Duration::from_secs(i)), None, "{i}");
        }
        // a timeout alarm ends once its timeouts left the window
        k.alarms.insert(("m".into(), Kind::Timeouts), "x".into());
        k.timeouts.insert("m".into(), VecDeque::from(vec![t0; TIMEOUTS]));
        assert!(k.expire(t0).is_empty());
        assert_eq!(k.expire(t0 + TIMEOUT_WINDOW + Duration::from_secs(1)), vec!["m".to_string()]);
        assert!(k.alarms.is_empty());
    }

    /// A lag or sync alarm of a named node ends only after a minute of fine answers without a
    /// break. A node hovering at the bound does not flap. Another kind ends with its finding.
    #[test]
    fn lag_and_sync_alarms_end_after_a_minute_of_fine_answers() {
        let mut j = Judge::default();
        let t0 = Instant::now();
        let s = |sec: u64| t0 + Duration::from_secs(sec);
        let lag = |w: Option<&str>| [(Kind::Lag, w.map(str::to_string))];
        assert_eq!(j.alarm("n", &lag(Some("behind")), t0).0, vec!["behind".to_string()]);
        assert!(j.alarm("n", &lag(None), s(10)).1.is_empty(), "one fine answer is not enough");
        assert!(j.alarm("n", &lag(None), s(50)).1.is_empty());
        // behind again: the minute begins again, and the alarm is not raised a second time
        let (began, ended) = j.alarm("n", &lag(Some("behind")), s(60));
        assert!(began.is_empty() && ended.is_empty());
        assert!(j.alarm("n", &lag(None), s(70)).1.is_empty());
        assert!(j.alarm("n", &lag(None), s(120)).1.is_empty());
        assert_eq!(j.alarm("n", &lag(None), s(130)).1, vec![Kind::Lag], "a minute of fine answers ends it");
        assert!(j.alarms.is_empty() && j.fine_since.is_empty());
        // another kind ends with its finding
        let net = |w: Option<&str>| [(Kind::Network, w.map(str::to_string))];
        j.alarm("n", &net(Some("other network")), s(200));
        assert_eq!(j.alarm("n", &net(None), s(201)).1, vec![Kind::Network]);
    }

    /// A named node is never replaced. Its findings raise alarms, which end with the findings. A
    /// public node's finding replaces it at once, and a node on another network is never used.
    #[test]
    fn a_named_node_raises_alarms_and_a_public_one_is_replaced() {
        let _g = resolver::tests::serial();
        resolver::tests::reset(|_, avoid| {
            let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh"];
            let pick = nodes.iter().find(|n| !avoid.contains(&resolver::host(n))).unwrap_or(&nodes[0]);
            Ok(resolver::Resolved { url: pick.to_string(), resolver: "fake".into() })
        });
        let named = "ws://judge-test.invalid:18210";
        assert_eq!(server_info(named, named, "testnet-11", true, true), Ok(()), "a named node's answer is used as it is");
        assert!(alarm_of(named).unwrap().contains("it is on testnet-11, not testnet-10"));
        assert!(alarms().iter().any(|a| a.starts_with("ws://judge-test.invalid:18210: it is on testnet-11")));
        assert_eq!(server_info(named, named, "testnet-10", true, true), Ok(()));
        assert_eq!(alarm_of(named), None, "the alarm ends with the finding");
        contradicted(named, named, "a hash");
        assert!(alarm_of(named).unwrap().contains("contradicted"));
        assert!(!cuts_off(named), "a contradiction is the pair's to drop, not a cut");
        lock().alarms.clear();

        let (a, b) = (resolver::endpoint("resolver").unwrap(), resolver::endpoint("resolver:2").unwrap());
        // the authority on another network: never used, the cross-check is the authority now
        let e = server_info("resolver", &a, "testnet-11", true, true).unwrap_err();
        assert!(e.contains("a.example") && e.contains("given up"), "{e}");
        assert_eq!(resolver::current("resolver"), Some(b.clone()));
        assert!(resolver::last_reason("resolver").unwrap().contains("a.example: it is on testnet-11"));
        // a public node that hangs: replaced at the third timeout
        let c = resolver::endpoint("resolver:2").unwrap();
        for _ in 0..TIMEOUTS - 1 {
            timed_out("resolver:2", &c);
        }
        assert_eq!(resolver::current("resolver:2"), Some(c.clone()));
        timed_out("resolver:2", &c);
        assert_eq!(resolver::current("resolver:2"), None);
        assert!(resolver::last_reason("resolver:2").unwrap().contains("hangs"));
        // a survivor that is behind is not promoted: the authority resolves a node of its own
        let d = resolver::endpoint("resolver:2").unwrap();
        lag("resolver:2", &d, Some(500), None);
        assert!(suspect(&d));
        resolver::replace("resolver", &b, "test");
        assert_eq!(resolver::current("resolver:2"), Some(d.clone()), "the suspect keeps its role");
        assert_eq!(resolver::current("resolver"), None);
        lag("resolver:2", &d, None, None);
        assert!(!suspect(&d));
        assert!(!alarms().iter().any(|a| a.contains(".example")), "public nodes raise no alarm, they are replaced");
    }
}
