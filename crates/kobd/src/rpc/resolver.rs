//! Public Kaspa nodes through the Kaspa resolvers: `rpc_url = "resolver"` (`--rpc-url resolver`), always as a pair of
//! nodes on two different hosts.
//!
//! The public node network is reached through resolvers (rusty-kaspa `rpc/wrpc/client/Resolvers.toml`, the groups
//! `*.kaspa.stream`, `*.kaspa.red`, `*.kaspa.green`, `*.kaspa.blue`), whose API (`rpc/wrpc/client/src/resolver.rs`) is
//! `GET {resolver}/v2/kaspa/{network}/{tls|any}/wrpc/{borsh|json}` answering `{"uid": ..., "url": "wss://.../wrpc/borsh"}`.
//! The public nodes serve Borsh wRPC only (`json` is `NOT FOUND` on every resolver, 2026-10-22), so a resolved node is always
//! Borsh over TLS and every call to it goes through `rpc::bridge`. Some resolvers are down at any time: they are tried in a
//! random order, each with a short timeout, until one answers.
//!
//! **Entries.** A configured `resolver` is a pseudo URL: `resolver` is the chain authority's entry, and
//! `index_cli::build_config` always adds a second one, `resolver:2`, the cross-checking node (`rpc::multi`: its acceptance data
//! is compared with the authority's before anything is indexed, and it takes every submission too); a further
//! `--node resolver` gets `resolver:3`, ... Every client of the process given an entry (the indexer's connections, the
//! matcher's, the x402 facilitator's, the setup's check) asks [`endpoint`] for the node it stands for, so all of them use the
//! same node per entry, and the entries use different hosts.
//!
//! **Failure.** A client that cannot connect to its entry's node, or loses the connection, reports it ([`failed`]). Public
//! nodes close connections now and then under load, so the first drop of a node gets a **grace reconnect**: the same node is
//! tried once more, with a short connect timeout ([`GRACE_CONNECT_TIMEOUT`]). When it is the authority's node, the other
//! node is the authority at once meanwhile (no resolution, no wait: the survivor keeps the indexer running) and the dropped
//! node gets its retry as the cross-checking one. A node that drops again within [`DROP_WINDOW`] of its last drop (the
//! retry failing included) is given up at once ([`replace`], also what a persistent disagreement does): the authority takes
//! the other entry's node over, and the entries left without a node resolve a replacement on a host different from the ones
//! in use and from the failed ones, in the background. When no node is left, every entry resolves again on its next
//! use, with a backoff (2 s doubling to 60 s) while the resolvers name none. Every change is logged with the hosts.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rand::seq::SliceRandom;

/// The configuration value that selects public nodes.
pub const RESOLVER: &str = "resolver";

/// The resolvers of rusty-kaspa `rpc/wrpc/client/Resolvers.toml` (the enabled groups, 2026-10-22).
pub const RESOLVERS: &[&str] = &[
    "https://eric.kaspa.stream",
    "https://maxim.kaspa.stream",
    "https://sean.kaspa.stream",
    "https://troy.kaspa.stream",
    "https://john.kaspa.red",
    "https://mike.kaspa.red",
    "https://paul.kaspa.red",
    "https://alex.kaspa.red",
    "https://jake.kaspa.green",
    "https://mark.kaspa.green",
    "https://adam.kaspa.green",
    "https://liam.kaspa.green",
    "https://noah.kaspa.blue",
    "https://ryan.kaspa.blue",
    "https://jack.kaspa.blue",
    "https://luke.kaspa.blue",
];

/// One resolver's answer may take this long.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Rounds over every resolver before a node on a host already in use is accepted (the resolvers spread their answers over
/// the nodes, so a second round usually names another host).
pub const ROUNDS: usize = 3;

/// The connect timeout of the one reconnect a dropped public node gets ([`failed`]).
pub const GRACE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A node that drops again within this long of its last drop is given up at once, without another reconnect.
pub const DROP_WINDOW: Duration = Duration::from_secs(120);

/// Whether a configured node URL is a resolver entry.
pub fn is_resolver(url: &str) -> bool {
    url == RESOLVER || url.strip_prefix("resolver:").is_some_and(|n| n.parse::<u32>().is_ok())
}

/// The pseudo URL of the `n`-th resolver entry (1: `resolver`, the authority).
pub fn pseudo(n: usize) -> String {
    if n <= 1 {
        RESOLVER.to_string()
    } else {
        format!("{RESOLVER}:{n}")
    }
}

/// The query for a public node of `network` that speaks Borsh over TLS.
pub fn query_url(resolver: &str, network: &str) -> String {
    format!("{}/v2/kaspa/{network}/tls/wrpc/borsh", resolver.trim_end_matches('/'))
}

/// The node URL of a resolver's answer (`{"uid": ..., "url": ...}`): a `wss://` URL (TLS was asked for).
pub fn parse_answer(body: &str) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct NodeDescriptor {
        url: String,
    }
    let d: NodeDescriptor = serde_json::from_str(body.trim()).map_err(|_| format!("not a node descriptor: {}", short(body)))?;
    if !d.url.starts_with("wss://") || crate::rpc::tls::target(&d.url).is_err() {
        return Err(format!("not a wss:// node URL: {}", short(&d.url)));
    }
    Ok(d.url)
}

/// The host of a node URL (what "a different node" compares); the URL itself when it has none.
pub fn host(url: &str) -> String {
    crate::rpc::tls::target(url).map(|t| t.host).unwrap_or_else(|_| url.to_string())
}

fn short(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(80).filter(|c| !c.is_control()).collect();
    if s.chars().count() > 80 {
        out.push('…');
    }
    out
}

/// A node the resolvers named, and which resolver named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub url: String,
    pub resolver: String,
}

/// Asks the resolvers in `order` for a node of `network`, one after the other, up to [`ROUNDS`] times over the list, until one
/// names a node whose host is not in `avoid_hosts`; when every answer is on such a host, the first of them (there is no
/// other). `get` is the HTTP GET (`tls::http_get`).
pub fn resolve_in(
    order: &[String],
    network: &str,
    avoid_hosts: &[String],
    mut get: impl FnMut(&str) -> Result<(u16, String), String>,
) -> Result<Resolved, String> {
    let mut failures = vec![];
    let mut fallback = None;
    for round in 0..ROUNDS {
        let mut answered = false;
        for r in order {
            let answer = get(&query_url(r, network)).and_then(|(status, body)| match status {
                200 => parse_answer(&body),
                s => Err(format!("HTTP {s}: {}", short(&body))),
            });
            match answer {
                Ok(url) if avoid_hosts.contains(&host(&url)) => {
                    answered = true;
                    fallback.get_or_insert(Resolved { url, resolver: r.clone() });
                }
                Ok(url) => return Ok(Resolved { url, resolver: r.clone() }),
                Err(e) if round == 0 => failures.push(format!("{r}: {e}")),
                Err(_) => {}
            }
        }
        // no resolver answers at all: another round asks the same ones in vain
        if !answered {
            break;
        }
    }
    fallback.ok_or_else(|| format!("no Kaspa resolver named a {network} node ({})", failures.join("; ")))
}

/// [`resolve_in`] over [`RESOLVERS`] (or the advanced override `$KOB_RESOLVERS`, comma-separated resolver URLs, for an operator
/// who runs a private resolver over its own nodes) in a random order, with
/// real HTTP.
pub fn resolve(network: &str, avoid_hosts: &[String]) -> Result<Resolved, String> {
    let mut order: Vec<String> = match std::env::var("KOB_RESOLVERS") {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => RESOLVERS.iter().map(|s| s.to_string()).collect(),
    };
    order.shuffle(&mut rand::thread_rng());
    resolve_in(&order, network, avoid_hosts, |u| crate::rpc::tls::http_get(u, QUERY_TIMEOUT))
}

/// How the state resolves a node (real HTTP; tests put a fake in).
type ResolveFn = fn(&str, &[String]) -> Result<Resolved, String>;

/// One entry's resolution backoff.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    until: Instant,
    delay: Duration,
}

/// The nodes the process uses through its resolver entries.
struct State {
    network: String,
    /// entry -> the node it uses now
    current: HashMap<String, String>,
    /// hosts that failed recently (avoided by the next resolutions; the oldest is forgotten first)
    failed: VecDeque<String>,
    /// entry -> how many times its node was given up
    replaced: HashMap<String, u32>,
    /// entry -> no resolution before this (the resolvers named nothing lately)
    backoff: HashMap<String, Backoff>,
    /// entries resolving in the background now (one thread per entry at a time)
    pending: Vec<String>,
    resolve: ResolveFn,
    /// replacements are resolved on a thread of their own (off in unit tests, which drive [`endpoint`] themselves)
    background: bool,
    /// host -> when it last dropped a connection (its grace reconnect, [`failed`])
    dropped: HashMap<String, Instant>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(State {
            network: String::new(),
            current: HashMap::new(),
            failed: VecDeque::new(),
            replaced: HashMap::new(),
            backoff: HashMap::new(),
            pending: vec![],
            resolve,
            background: true,
            dropped: HashMap::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// The network the resolver entries resolve for (the configuration's, `index_cli::build_config`; the setup's).
pub fn set_network(network: &str) {
    let mut s = lock();
    if s.network != network {
        s.network = network.to_string();
        s.current.clear();
        s.backoff.clear();
    }
}

/// The node a resolver entry uses now, without resolving (`None`: none yet, or it is being replaced).
pub fn current(url: &str) -> Option<String> {
    lock().current.get(url).cloned()
}

/// How many times a resolver entry gave its node up (failures and replacements).
pub fn replacements(url: &str) -> u32 {
    lock().replaced.get(url).copied().unwrap_or(0)
}

/// The node behind a configured URL: the URL itself, or for a resolver entry the public node it uses now (resolved on first
/// use and after its node failed, on a host no other entry uses and that did not fail lately). Blocking (HTTP to the
/// resolvers): async callers run it on the blocking pool.
pub fn endpoint(url: &str) -> Result<String, String> {
    if !is_resolver(url) {
        return Ok(url.to_string());
    }
    // the view of the other entries may change while the resolvers are asked (a failover, another client's resolution):
    // a node another entry took meanwhile is not taken a second time, the resolution is repeated on the new view
    for attempt in 1..=3 {
        let (network, avoid, resolve_fn) = {
            let s = lock();
            if let Some(u) = s.current.get(url) {
                return Ok(u.clone());
            }
            if s.network.is_empty() {
                return Err("a resolver node needs the network (set before connecting)".into());
            }
            if let Some(b) = s.backoff.get(url).filter(|b| b.until > Instant::now()) {
                let wait = b.until.saturating_duration_since(Instant::now()).as_secs() + 1;
                return Err(format!("no public node yet (the resolvers named none; again in {wait} s)"));
            }
            let mut avoid: Vec<String> = s.failed.iter().cloned().collect();
            avoid.extend(in_use(&s, url));
            (s.network.clone(), avoid, s.resolve)
        };
        let r = resolve_fn(&network, &avoid);
        let mut s = lock();
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                let delay = s.backoff.get(url).map_or(Duration::from_secs(2), |b| (b.delay * 2).min(Duration::from_secs(60)));
                s.backoff.insert(url.to_string(), Backoff { until: Instant::now() + delay, delay });
                tracing::warn!(entry = url, error = %e, retry_secs = delay.as_secs(), "no public Kaspa node");
                return Err(e);
            }
        };
        s.backoff.remove(url);
        // another client of the same entry may have resolved meanwhile: keep one node per entry
        if let Some(u) = s.current.get(url) {
            return Ok(u.clone());
        }
        let h = host(&r.url);
        if in_use(&s, url).contains(&h) && !avoid.contains(&h) && attempt < 3 {
            continue; // taken by another entry while the resolvers were asked
        }
        if avoid.contains(&h) || in_use(&s, url).contains(&h) {
            tracing::warn!(entry = url, node = %h, "the resolvers name no other public node: this one failed lately or another entry uses it");
        }
        tracing::info!(entry = url, node = %h, resolver = %r.resolver, network = %network, role = role(url), "public Kaspa node (via the Kaspa resolver)");
        s.current.insert(url.to_string(), r.url.clone());
        return Ok(r.url);
    }
    unreachable!("the last attempt returns")
}

/// The hosts the entries other than `url` use now.
fn in_use(s: &State, url: &str) -> Vec<String> {
    s.current.iter().filter(|(k, _)| k.as_str() != url).map(|(_, v)| host(v)).collect()
}

fn role(url: &str) -> &'static str {
    if url == RESOLVER {
        "chain authority"
    } else {
        "cross-check"
    }
}

/// A connection to `node` (the node the resolver entry `url` stood for) failed or was lost. Its first drop within
/// [`DROP_WINDOW`] gets one reconnect to the same node (module documentation: the authority's role moves to the other node
/// meanwhile, and the dropped node is retried as the cross-checking one); a second drop within the window gives it up
/// ([`replace`]). A report about a node the entry no longer uses is ignored (another client reported it first).
pub fn failed(url: &str, node: &str) {
    if !is_resolver(url) {
        return;
    }
    {
        let mut s = lock();
        if s.current.get(url).map(String::as_str) != Some(node) {
            return;
        }
        let h = host(node);
        let again = s.dropped.get(&h).is_some_and(|t| t.elapsed() < DROP_WINDOW);
        if !again {
            s.dropped.insert(h.clone(), Instant::now());
            let survivor = (url == RESOLVER)
                .then(|| s.current.iter().find(|(k, v)| k.as_str() != RESOLVER && host(v) != h).map(|(k, v)| (k.clone(), v.clone())))
                .flatten();
            match survivor {
                Some((entry, n)) => {
                    // the other node is the authority at once; the dropped one gets its retry as the cross-check
                    s.current.insert(RESOLVER.to_string(), n.clone());
                    s.current.insert(entry.clone(), node.to_string());
                    tracing::warn!(dropped = %h, authority = %host(&n), "public node dropped the connection: the other node is the chain authority now; reconnecting once to the dropped one as the cross-check");
                }
                None => tracing::warn!(entry = url, dropped = %h, "public node dropped the connection: reconnecting once"),
            }
            return;
        }
    }
    replace(url, node);
}

/// The connect timeout of a client of the resolver entry `url`: [`GRACE_CONNECT_TIMEOUT`] while its node is on its grace
/// reconnect (it dropped within [`DROP_WINDOW`]), else `default`.
pub fn connect_timeout(url: &str, default: Duration) -> Duration {
    if !is_resolver(url) {
        return default;
    }
    let s = lock();
    let dropped = s.current.get(url).and_then(|n| s.dropped.get(&host(n))).is_some_and(|t| t.elapsed() < DROP_WINDOW);
    if dropped {
        default.min(GRACE_CONNECT_TIMEOUT)
    } else {
        default
    }
}

/// The resolver entry `url` gives `node` up at once (a second drop within [`DROP_WINDOW`], or a disagreement that does not
/// settle, `rpc::multi`): the authority's entry takes over the node of another entry when there is one, and the entries
/// without a node resolve replacements in the background (hosts different from the ones in use and from the failed ones). A
/// report about a node the entry no longer uses is ignored.
pub fn replace(url: &str, node: &str) {
    if !is_resolver(url) {
        return;
    }
    let refill = {
        let mut s = lock();
        if s.current.get(url).map(String::as_str) != Some(node) {
            return;
        }
        s.current.remove(url);
        *s.replaced.entry(url.to_string()).or_default() += 1;
        let h = host(node);
        if !s.failed.contains(&h) {
            s.failed.push_back(h.clone());
            while s.failed.len() > 8 {
                s.failed.pop_front();
            }
        }
        // another entry on the same host lost its node too
        let same: Vec<String> = s.current.iter().filter(|(_, v)| host(v) == h).map(|(k, _)| k.clone()).collect();
        for k in &same {
            s.current.remove(k);
        }
        // the entries left without a node: this one (or the survivor's once it is promoted) and those on the same host
        let mut need = vec![url.to_string()];
        need.extend(same);
        if url == RESOLVER {
            // the survivor becomes the chain authority at once; its entry resolves a replacement
            let survivor =
                s.current.iter().filter(|(k, v)| k.as_str() != RESOLVER && host(v) != h).map(|(k, v)| (k.clone(), v.clone())).min();
            match survivor {
                Some((entry, n)) => {
                    s.current.remove(&entry);
                    s.current.insert(RESOLVER.to_string(), n.clone());
                    tracing::warn!(failed = %h, authority = %host(&n), from = %entry, "public node failed: the other node is the chain authority now; resolving a replacement");
                    need[0] = entry;
                }
                None => tracing::warn!(failed = %h, "public node failed and no other is connected: resolving again"),
            }
        } else {
            tracing::warn!(entry = url, failed = %h, "public node failed: resolving a replacement");
        }
        need.sort();
        need.dedup();
        let missing: Vec<String> = need.into_iter().filter(|e| !s.pending.contains(e) && !s.current.contains_key(e)).collect();
        if s.background {
            s.pending.extend(missing.iter().cloned());
            missing
        } else {
            vec![]
        }
    };
    for entry in refill {
        std::thread::spawn(move || {
            let _ = endpoint(&entry);
            lock().pending.retain(|e| *e != entry);
        });
    }
}

/// The resolver entries in use and their nodes' hosts, for the status line (`authority host, cross-check host`).
pub fn describe() -> Vec<(String, Option<String>, u32)> {
    let s = lock();
    let mut entries: Vec<String> = s.current.keys().chain(s.replaced.keys()).cloned().collect();
    entries.sort_by_key(|e| (e != RESOLVER, e.clone()));
    entries.dedup();
    entries.into_iter().map(|e| (e.clone(), s.current.get(&e).map(|n| host(n)), s.replaced.get(&e).copied().unwrap_or(0))).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The resolver state is process-wide: its tests run one at a time.
    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        static M: Mutex<()> = Mutex::new(());
        M.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fake resolution: the first of `FAKE_NODES` whose host is not avoided, else the first.
    fn fake_resolve(_network: &str, avoid: &[String]) -> Result<Resolved, String> {
        let nodes = ["wss://a.example/x/borsh", "wss://b.example/x/borsh", "wss://c.example/x/borsh", "wss://d.example/x/borsh"];
        let pick = nodes.iter().find(|n| !avoid.contains(&host(n))).unwrap_or(&nodes[0]);
        Ok(Resolved { url: pick.to_string(), resolver: "fake".into() })
    }

    pub(crate) fn reset(resolve: ResolveFn) {
        let mut s = lock();
        s.network = "testnet-10".into();
        s.current.clear();
        s.failed.clear();
        s.replaced.clear();
        s.backoff.clear();
        s.pending.clear();
        s.dropped.clear();
        s.resolve = resolve;
        s.background = false;
    }

    #[test]
    fn urls_and_answers() {
        assert_eq!(
            query_url("https://eric.kaspa.stream/", "testnet-10"),
            "https://eric.kaspa.stream/v2/kaspa/testnet-10/tls/wrpc/borsh"
        );
        assert_eq!(query_url("http://127.0.0.1:8888", "mainnet"), "http://127.0.0.1:8888/v2/kaspa/mainnet/tls/wrpc/borsh");
        assert_eq!(
            parse_answer(r#"{"uid":"b5061c7c25dd77ba","url":"wss://muon-10.kaspa.blue/kaspa/testnet-10/wrpc/borsh"}"#).unwrap(),
            "wss://muon-10.kaspa.blue/kaspa/testnet-10/wrpc/borsh"
        );
        for bad in ["NOT FOUND", "", "{}", r#"{"url":"ws://plain/x"}"#, r#"{"url":"wss://"}"#, "error code: 521"] {
            assert!(parse_answer(bad).is_err(), "{bad}");
        }
        assert!(is_resolver("resolver") && is_resolver("resolver:2"));
        assert!(!is_resolver("resolver:x") && !is_resolver("ws://resolver") && !is_resolver(""));
        assert_eq!((pseudo(1), pseudo(2)), ("resolver".to_string(), "resolver:2".to_string()));
        assert!(crate::rpc::bridge::is_borsh_url(
            &parse_answer(r#"{"url":"wss://iris.kaspa.blue/kaspa/mainnet/wrpc/borsh"}"#).unwrap()
        ));
        assert_eq!(host("wss://iris.kaspa.blue/kaspa/mainnet/wrpc/borsh"), "iris.kaspa.blue");
        assert_eq!(RESOLVERS.len(), 16);
    }

    #[test]
    fn resolvers_are_tried_until_one_names_a_node_on_another_host() {
        let order: Vec<String> = ["https://a", "https://b", "https://c", "https://d"].iter().map(|s| s.to_string()).collect();
        let node = |n: &str| format!(r#"{{"uid":"1","url":"wss://{n}.example/kaspa/testnet-10/wrpc/borsh"}}"#);
        let answers = |u: &str| -> Result<(u16, String), String> {
            match &u[..9] {
                "https://a" => Err("connect: refused".into()),
                "https://b" => Ok((521, "error code: 521".into())),
                "https://c" => Ok((200, node("one"))),
                _ => Ok((200, node("two"))),
            }
        };
        let mut asked = vec![];
        let r = resolve_in(&order, "testnet-10", &[], |u| {
            asked.push(u.to_string());
            answers(u)
        })
        .unwrap();
        assert_eq!(r, Resolved { url: "wss://one.example/kaspa/testnet-10/wrpc/borsh".into(), resolver: "https://c".into() });
        assert_eq!(asked.len(), 3);
        // the host in use is avoided while another is named
        let r = resolve_in(&order, "testnet-10", &["one.example".into()], answers).unwrap();
        assert_eq!(r.url, "wss://two.example/kaspa/testnet-10/wrpc/borsh");
        // a resolver that names another host only in a later round (it spreads its answers): asked again
        let mut n = 0;
        let r = resolve_in(&order[2..3], "testnet-10", &["one.example".into()], |_| {
            n += 1;
            Ok((200, node(if n < 3 { "one" } else { "three" })))
        })
        .unwrap();
        assert_eq!((r.url.as_str(), n), ("wss://three.example/kaspa/testnet-10/wrpc/borsh", 3));
        // every answer on a host in use, every round: the first of them (there is no other)
        let both = ["one.example".to_string(), "two.example".into()];
        assert_eq!(resolve_in(&order, "testnet-10", &both, answers).unwrap().url, "wss://one.example/kaspa/testnet-10/wrpc/borsh");
        // none answers: every failure is named, one round only
        let mut calls = 0;
        let e = resolve_in(&order[..2], "mainnet", &[], |u| {
            calls += 1;
            answers(u)
        })
        .unwrap_err();
        assert!(
            e.contains("no Kaspa resolver named a mainnet node")
                && e.contains("https://a: connect: refused")
                && e.contains("HTTP 521"),
            "{e}"
        );
        assert_eq!(calls, 2);
    }

    /// The real HTTP path against local resolvers: one down (connection refused), one answering `NOT FOUND`, one naming a node.
    #[test]
    fn resolution_over_http_with_fallback() {
        use std::io::{Read, Write};
        let serve = |status: &'static str, body: &'static str| {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                while let Ok((mut c, _)) = l.accept() {
                    let mut b = [0u8; 1024];
                    let _ = c.read(&mut b);
                    let _ = c.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
                }
            });
            format!("http://127.0.0.1:{port}")
        };
        let down = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://127.0.0.1:{}", l.local_addr().unwrap().port()) // closed when `l` drops
        };
        let missing = serve("404 Not Found", "NOT FOUND");
        let good = serve("200 OK", r#"{"uid":"u","url":"wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh"}"#);
        let order = vec![down, missing.clone(), good.clone()];
        let r = resolve_in(&order, "testnet-10", &[], |u| crate::rpc::tls::http_get(u, Duration::from_secs(5))).unwrap();
        assert_eq!(r, Resolved { url: "wss://alpha-10.kaspa.stream/kaspa/testnet-10/wrpc/borsh".into(), resolver: good });
    }

    /// The pair: two entries resolve two different hosts; the authority's failure hands its role to the survivor at once
    /// (no resolution in between); the replacement is on a third host, neither the survivor's nor the failed one's.
    #[test]
    fn the_pair_fails_over_to_the_survivor_and_replaces_on_another_host() {
        let _g = serial();
        reset(fake_resolve);
        let a = endpoint("resolver").unwrap();
        let b = endpoint("resolver:2").unwrap();
        assert_eq!((host(&a), host(&b)), ("a.example".to_string(), "b.example".to_string()), "two different hosts");
        assert_eq!(endpoint("resolver").unwrap(), a, "one node per entry for every client");
        assert_eq!(endpoint("ws://127.0.0.1:18210").unwrap(), "ws://127.0.0.1:18210");
        // a report about a node the entry no longer uses: ignored
        replace("resolver", "wss://z.example/x/borsh");
        assert_eq!(current("resolver"), Some(a.clone()));
        // the authority fails: the survivor is the authority immediately, the cross-check entry has no node until replaced
        replace("resolver", &a);
        assert_eq!(current("resolver"), Some(b.clone()), "failover to the survivor with no resolution");
        assert_eq!(current("resolver:2"), None);
        let c = endpoint("resolver:2").unwrap();
        assert_eq!(host(&c), "c.example", "the replacement differs from the survivor and from the failed host");
        assert_eq!((replacements("resolver"), replacements("resolver:2")), (1, 0));
        // the cross-check node fails: the authority keeps its node, the replacement avoids every host seen failing or in use
        replace("resolver:2", &c);
        assert_eq!(current("resolver"), Some(b.clone()));
        assert_eq!(host(&endpoint("resolver:2").unwrap()), "d.example");
        let d = describe();
        assert_eq!(d[0], ("resolver".to_string(), Some("b.example".to_string()), 1));
        assert_eq!(d[1], ("resolver:2".to_string(), Some("d.example".to_string()), 1));
    }

    /// A node another entry took while the resolvers were asked is not taken a second time; a failed host takes every entry
    /// on it down, and the authority is never handed a node on the failed host.
    #[test]
    fn two_entries_never_end_on_one_host() {
        let _g = serial();
        // the first answer is a host another entry takes meanwhile (as a failover would)
        fn racing(_: &str, avoid: &[String]) -> Result<Resolved, String> {
            let mut s = lock();
            if !s.current.contains_key("resolver") {
                s.current.insert("resolver".into(), "wss://a.example/x/borsh".into());
                return Ok(Resolved { url: "wss://a.example/x/borsh".into(), resolver: "fake".into() });
            }
            drop(s);
            fake_resolve("testnet-10", avoid)
        }
        reset(racing);
        let b = endpoint("resolver:2").unwrap();
        assert_eq!(host(&b), "b.example", "not the host the authority took meanwhile");
        // both entries on one host (an old race, or the resolvers had no other): its failure clears both, nobody is promoted
        lock().current.insert("resolver:2".into(), "wss://a.example/x/borsh".into());
        replace("resolver", "wss://a.example/x/borsh");
        assert_eq!((current("resolver"), current("resolver:2")), (None, None));
    }

    /// A dropped connection gets one reconnect to the same node, with a short connect timeout: the authority's role moves to
    /// the other node at once and the dropped node is retried as the cross-check; a second drop within the window gives it up
    /// (a node on a third host replaces it); after the window a drop is a first drop again.
    #[test]
    fn a_first_drop_reconnects_once_a_second_one_replaces() {
        let _g = serial();
        reset(fake_resolve);
        let a = endpoint("resolver").unwrap();
        let b = endpoint("resolver:2").unwrap();
        let long = Duration::from_secs(15);
        failed("resolver", &a);
        assert_eq!((current("resolver"), current("resolver:2")), (Some(b.clone()), Some(a.clone())), "roles swapped, no resolution");
        assert_eq!(replacements("resolver") + replacements("resolver:2"), 0, "nothing replaced yet");
        assert_eq!(connect_timeout("resolver:2", long), GRACE_CONNECT_TIMEOUT, "the retry has a short connect timeout");
        assert_eq!(connect_timeout("resolver", long), long);
        assert_eq!(connect_timeout("ws://127.0.0.1:18210", long), long);
        // the retry fails (or it drops again within the window): given up, replaced on a third host
        failed("resolver:2", &a);
        assert_eq!(current("resolver"), Some(b.clone()));
        assert_eq!(current("resolver:2"), None);
        let c = endpoint("resolver:2").unwrap();
        assert_eq!(host(&c), "c.example");
        assert_eq!(replacements("resolver:2"), 1);
        // the cross-checking node's first drop: kept for its retry; its second: replaced
        failed("resolver:2", &c);
        assert_eq!(current("resolver:2"), Some(c.clone()));
        failed("resolver:2", &c);
        assert_eq!(current("resolver:2"), None);
        // a drop after the window is a first drop again
        let d = endpoint("resolver:2").unwrap();
        lock().dropped.insert(host(&d), Instant::now() - DROP_WINDOW - Duration::from_secs(1));
        failed("resolver:2", &d);
        assert_eq!(current("resolver:2"), Some(d.clone()), "a reconnect again, the window has passed");
    }

    /// Both nodes gone and the resolvers silent: every entry backs off (no resolution storm), then resolves again.
    #[test]
    fn with_no_node_left_resolution_backs_off() {
        let _g = serial();
        fn none(_: &str, _: &[String]) -> Result<Resolved, String> {
            Err("no Kaspa resolver named a testnet-10 node".into())
        }
        reset(none);
        assert!(endpoint("resolver").unwrap_err().contains("no Kaspa resolver"));
        let e = endpoint("resolver").unwrap_err();
        assert!(e.contains("again in"), "{e}");
        lock().resolve = fake_resolve;
        assert!(endpoint("resolver").is_err(), "still backing off");
        lock().backoff.clear();
        assert_eq!(host(&endpoint("resolver").unwrap()), "a.example");
    }
}
