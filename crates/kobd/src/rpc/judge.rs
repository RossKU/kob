//! Judging a node by its answers, not only by its connection.
//!
//! A node can accept the connection and still be useless: on another network, without the UTXO index the Executor reads its
//! funds from, not synced, hanging (its requests time out), behind the other node of the pair (`rpc::multi`, its health
//! watch), or contradicting it. Each of these counts as a failure of the node, like a lost connection, and the same checks
//! apply to every node of the pair, the chain authority and the cross-checking node alike:
//!
//! * a **resolver entry** (`rpc::resolver`, a public node) gives the node up for another at once ([`resolver::replace`],
//!   logged as a warning with the reason, which `/v1/health` shows per node as `cut_reason`): on another network, without the
//!   UTXO index (when the process needs it, [`require_utxo_index`]), reporting not synced for [`UNSYNCED_GRACE`],
//!   [`TIMEOUTS`] request timeouts within [`TIMEOUT_WINDOW`], behind the other node for a minute, or contradicting it;
//! * a **node the operator named** (a fixed URL) cannot be replaced: the same findings raise an **alarm** instead. It is
//!   logged as an error when it begins, listed under `node_alarms` in `/v1/health` (and as the node's `cut_reason`), counted
//!   by `kob_indexer_node_alarms` and appended to the status line, until the node is fine again. A cross-checking node with an
//!   alarm is left out of the pair meanwhile (the authority carries on alone; one that contradicted the authority is
//!   dropped by that pair until a restart, as before); the authority itself stays the authority.
//!
//! A VSPC request that times out is not counted: the batch windows adapt to a slow link (`rpc::window`). The small calls
//! (`getServerInfo`, `getBlock`, `getSinkBlueScore`, ...) are what a hanging node fails.

use super::{redact_url, resolver};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A node that reports not synced for this long is given up (public) or raises an alarm (named). A public node reports not
/// synced for a few seconds now and then; a node that has fallen out of the network for minutes does not come back soon.
pub const UNSYNCED_GRACE: Duration = Duration::from_secs(120);

/// Request timeouts of one node within [`TIMEOUT_WINDOW`] that make it a hanging node.
pub const TIMEOUTS: usize = 3;
pub const TIMEOUT_WINDOW: Duration = Duration::from_secs(600);

/// What a finding is about: one alarm per named node and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Network,
    UtxoIndex,
    Sync,
    Timeouts,
    Lag,
    Contradiction,
}

#[derive(Default)]
struct Judge {
    /// node -> since when it reports not synced
    unsynced: HashMap<String, Instant>,
    /// node -> its recent request timeouts (oldest first)
    timeouts: HashMap<String, VecDeque<Instant>>,
    /// nodes behind the other node of their pair at the last health check
    behind: HashSet<String>,
    /// (named node, kind) -> what is wrong
    alarms: BTreeMap<(String, Kind), String>,
}

impl Judge {
    /// Why `node` counts as not synced now (`None`: synced, or not for long enough yet).
    fn sync(&mut self, node: &str, synced: bool, now: Instant) -> Option<String> {
        if synced {
            self.unsynced.remove(node);
            return None;
        }
        let since = *self.unsynced.entry(node.to_string()).or_insert(now);
        (now.saturating_duration_since(since) >= UNSYNCED_GRACE)
            .then(|| format!("it has reported not synced for over {} min", UNSYNCED_GRACE.as_secs() / 60))
    }

    /// A request to `node` timed out: why it counts as hanging (`None`: not often enough yet).
    fn timed_out(&mut self, node: &str, now: Instant) -> Option<String> {
        let q = self.timeouts.entry(node.to_string()).or_default();
        q.push_back(now);
        prune(q, now);
        (q.len() >= TIMEOUTS).then(|| format!("{TIMEOUTS} requests timed out within {} min (it hangs)", TIMEOUT_WINDOW.as_secs() / 60))
    }

    /// Everything known about a node that was given up (its successor starts clean).
    fn forget(&mut self, node: &str) {
        self.unsynced.remove(node);
        self.timeouts.remove(node);
        self.behind.remove(node);
    }

    /// Timeout alarms whose timeouts left the window end.
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

/// The process needs the UTXO index of its nodes (`kobd --executor`: the operator's funds are read there): a node without it
/// is a finding from now on.
pub fn require_utxo_index() {
    NEED_UTXO_INDEX.store(true, Ordering::Relaxed);
}

/// Acts on the findings about `node`, the node the configured `url` stands for: the first finding gives a resolver entry's
/// node up (it is returned); a named node's alarms begin and end with them. Never called with the judge's lock held, since
/// [`resolver::replace`] asks [`suspect`].
fn act(url: &str, node: &str, findings: &[(Kind, Option<String>)]) -> Option<(Kind, String)> {
    if resolver::is_resolver(url) {
        let bad = findings.iter().find_map(|(k, w)| w.clone().map(|w| (*k, w)));
        if let Some((_, why)) = &bad {
            // a report about a node the entry no longer uses changes nothing (another client reported it first)
            if resolver::current(url).as_deref() == Some(node) {
                lock().forget(node);
                resolver::replace(url, node, why);
            }
        }
        return bad;
    }
    let (mut began, mut ended) = (vec![], vec![]);
    {
        let mut j = lock();
        for (kind, why) in findings {
            let key = (url.to_string(), *kind);
            match why {
                Some(w) => {
                    if j.alarms.insert(key, w.clone()).is_none() {
                        began.push(w.clone());
                    }
                }
                None => {
                    if j.alarms.remove(&key).is_some() {
                        ended.push(*kind);
                    }
                }
            }
        }
    }
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

/// A `getServerInfo` answer of `node` (the node the configured `url` stands for now). `Err`: the answer must not be used, the
/// public node is on another network and was given up (the caller retries on its successor). A named node on another network
/// raises an alarm and its answer is used as it is (the follower and the runner refuse another network themselves).
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

/// A small request to `node` timed out (not a VSPC batch: those adapt, `rpc::window`).
pub fn timed_out(url: &str, node: &str) {
    let why = lock().timed_out(node, Instant::now());
    if why.is_some() {
        act(url, node, &[(Kind::Timeouts, why)]);
    }
}

/// The pair's health watch (`rpc::multi`) compared `node` with the other node: `behind` (DAA) when it is behind by more than
/// the watch's bound now, and `sustained` once it has been for the watch's whole period (a finding). `None` clears it.
pub fn lag(url: &str, node: &str, behind: Option<u64>, sustained: bool) {
    {
        let mut j = lock();
        if behind.is_some() {
            j.behind.insert(node.to_string());
        } else {
            j.behind.remove(node);
        }
    }
    match behind {
        None => {
            act(url, node, &[(Kind::Lag, None)]);
        }
        Some(d) if sustained => {
            act(url, node, &[(Kind::Lag, Some(format!("it stays {d} DAA behind the other node (about {} s of chain)", d / 10)))]);
        }
        Some(_) => {}
    }
}

/// `node` contradicted the other node of the pair (`rpc::multi`: a hash or the acceptance data). A named node's alarm stays
/// until the process restarts (it is dropped from the pair for good, as before).
pub fn contradicted(url: &str, node: &str, why: &str) {
    act(url, node, &[(Kind::Contradiction, Some(format!("it contradicted the other node: {why}")))]);
}

/// Whether the judge suspects `node` now (it reports not synced, or it was behind the other node at the last check): such a
/// node is not promoted to chain authority (`rpc::resolver`).
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

/// Whether the named node `url` has an alarm that cuts it off from a pair now (`rpc::multi`). A contradiction does not: the
/// pair that saw it drops the node itself (until the process restarts), and another pair over the same URL judges it on what
/// it sees.
pub fn cuts_off(url: &str) -> bool {
    expire();
    lock().alarms.keys().any(|(u, k)| u == url && *k != Kind::Contradiction)
}

/// The alarms of the named node `url` (`None`: none), joined: what `/v1/health` shows as its `cut_reason`.
pub fn alarm_of(url: &str) -> Option<String> {
    expire();
    let j = lock();
    let v: Vec<&str> = j.alarms.iter().filter(|((u, _), _)| u == url).map(|(_, w)| w.as_str()).collect();
    (!v.is_empty()).then(|| v.join("; "))
}

/// Every alarm of a named node now, `node: problem` (the URL redacted): `node_alarms` of `/v1/health`.
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

    /// A named node is never replaced: its findings raise alarms, which end with the findings; a public node's finding
    /// replaces it at once, and a node on another network is never used.
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
        lag("resolver:2", &d, Some(500), false);
        assert!(suspect(&d));
        resolver::replace("resolver", &b, "test");
        assert_eq!(resolver::current("resolver:2"), Some(d.clone()), "the suspect keeps its role");
        assert_eq!(resolver::current("resolver"), None);
        lag("resolver:2", &d, None, false);
        assert!(!suspect(&d));
        assert!(!alarms().iter().any(|a| a.contains(".example")), "public nodes raise no alarm, they are replaced");
    }
}
