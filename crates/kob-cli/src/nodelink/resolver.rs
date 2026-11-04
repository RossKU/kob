//! Public Kaspa nodes through the Kaspa resolvers (the same list and protocol as the executor's `rpc::resolver`): the
//! resolvers (rusty-kaspa `rpc/wrpc/client/Resolvers.toml`) answer `GET {resolver}/v2/kaspa/{network}/tls/wrpc/borsh` with
//! `{"uid": .., "url": "wss://.../wrpc/borsh"}`. The public nodes serve Borsh wRPC only. Some resolvers are down at any time:
//! they are tried in a random order, each with a short timeout, until one answers. A client uses a pair of nodes on two
//! different hosts ([`super::pair`]).

use std::time::Duration;

use rand::seq::SliceRandom;
use serde_json::Value;

/// The `--node` value (and the default) that selects public nodes.
pub const RESOLVER: &str = "resolver";

/// The resolvers of rusty-kaspa `rpc/wrpc/client/Resolvers.toml` (the enabled groups; the executor's list).
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

/// Rounds over every resolver before a node on a host already in use is accepted (the resolvers spread their answers over the
/// nodes, so a second round usually names another host).
pub const ROUNDS: usize = 3;

/// The network a resolver URL stands for: `resolver:<network>` (the node setting the CLI keeps once the network is known).
pub fn network_of(url: &str) -> Option<&str> {
    url.strip_prefix("resolver:").filter(|n| !n.is_empty())
}

/// The pseudo node URL of `network`.
pub fn pseudo(network: &str) -> String {
    format!("{RESOLVER}:{network}")
}

/// The path of the query for a public node of `network` that speaks Borsh over TLS.
pub fn query_path(network: &str) -> String {
    format!("/v2/kaspa/{network}/tls/wrpc/borsh")
}

/// The node URL of a resolver's answer (`{"uid": .., "url": ..}`): a `wss://` URL (TLS was asked for).
pub fn parse_answer(v: &Value) -> Result<String, String> {
    let url = v.get("url").and_then(Value::as_str).ok_or_else(|| format!("not a node descriptor: {}", short(&v.to_string())))?;
    if !url.starts_with("wss://") || super::client::target(url).is_err() {
        return Err(format!("not a wss:// node URL: {}", short(url)));
    }
    Ok(url.to_string())
}

/// The host of a node URL (what "a different node" compares); the URL itself when it has none.
pub fn host(url: &str) -> String {
    super::client::target(url).map(|t| t.host).unwrap_or_else(|_| url.to_string())
}

fn short(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(80).filter(|c| !c.is_control()).collect();
    if s.chars().count() > 80 {
        out.push('~');
    }
    out
}

/// A node the resolvers named, and which resolver named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The node's `wss://` URL.
    pub url: String,
    /// The resolver that named it.
    pub resolver: String,
}

/// Asks the resolvers in `order` for a node of `network`, one after the other, up to [`ROUNDS`] times over the list, until one
/// names a node whose host is not in `avoid_hosts`; when every answer is on such a host, the first of them (there is no other).
/// `get(resolver, path)` is the HTTP GET.
pub fn resolve_in(
    order: &[String],
    network: &str,
    avoid_hosts: &[String],
    mut get: impl FnMut(&str, &str) -> Result<(u16, Value), String>,
) -> Result<Resolved, String> {
    let mut failures = vec![];
    let mut fallback = None;
    for round in 0..ROUNDS {
        let mut answered = false;
        for r in order {
            let answer = get(r, &query_path(network)).and_then(|(status, body)| match status {
                200 => parse_answer(&body),
                s => Err(format!("HTTP {s}: {}", short(&body.to_string()))),
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

/// [`resolve_in`] over [`RESOLVERS`] (or `$KOB_RESOLVERS`, comma-separated resolver URLs, for an operator who runs a private
/// resolver over its own nodes) in a random order, with real HTTP.
pub fn resolve(network: &str, avoid_hosts: &[String]) -> Result<Resolved, String> {
    let mut order: Vec<String> = match std::env::var("KOB_RESOLVERS") {
        Ok(v) if !v.trim().is_empty() => v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        _ => RESOLVERS.iter().map(|s| s.to_string()).collect(),
    };
    order.shuffle(&mut rand::thread_rng());
    resolve_in(&order, network, avoid_hosts, |r, path| crate::trade::http::get(r, path, QUERY_TIMEOUT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(host: &str) -> Value {
        json!({"uid": "x", "url": format!("wss://{host}/kaspa/mainnet/wrpc/borsh")})
    }

    #[test]
    fn pseudo_urls_and_answers() {
        assert_eq!(pseudo("mainnet"), "resolver:mainnet");
        assert_eq!(network_of("resolver:testnet-10"), Some("testnet-10"));
        assert_eq!(network_of("resolver"), None);
        assert_eq!(network_of("ws://h"), None);
        assert_eq!(query_path("mainnet"), "/v2/kaspa/mainnet/tls/wrpc/borsh");
        assert_eq!(parse_answer(&node("a.example")).unwrap(), "wss://a.example/kaspa/mainnet/wrpc/borsh");
        assert!(parse_answer(&json!({"url": "ws://a.example/borsh"})).is_err());
        assert!(parse_answer(&json!({"nope": 1})).is_err());
        assert_eq!(host("wss://a.example/kaspa/mainnet/wrpc/borsh"), "a.example");
    }

    #[test]
    fn the_first_answering_resolver_wins_and_other_hosts_are_preferred() {
        let order: Vec<String> = ["https://r1", "https://r2", "https://r3"].iter().map(|s| s.to_string()).collect();
        // r1 is down, r2 answers
        let mut asked = vec![];
        let r = resolve_in(&order, "mainnet", &[], |r, p| {
            asked.push(r.to_string());
            assert_eq!(p, "/v2/kaspa/mainnet/tls/wrpc/borsh");
            if r == "https://r1" {
                Err("connection refused".into())
            } else {
                Ok((200, node(&format!("{}.example", &r[8..]))))
            }
        })
        .unwrap();
        assert_eq!((r.resolver.as_str(), r.url.as_str()), ("https://r2", "wss://r2.example/kaspa/mainnet/wrpc/borsh"));
        assert_eq!(asked, ["https://r1", "https://r2"]);
        // the host already in use is skipped while another resolver names another
        let r = resolve_in(&order, "mainnet", &["r1.example".to_string()], |r, _| Ok((200, node(&format!("{}.example", &r[8..])))))
            .unwrap();
        assert_eq!(r.resolver, "https://r2");
        // every resolver names the host in use: it is taken (there is no other), after the rounds
        let mut n = 0;
        let r = resolve_in(&order, "mainnet", &["same.example".to_string()], |_, _| {
            n += 1;
            Ok((200, node("same.example")))
        })
        .unwrap();
        assert_eq!((r.resolver.as_str(), n), ("https://r1", 3 * ROUNDS));
        // nobody answers: one round, every failure named
        let mut n = 0;
        let e = resolve_in(&order, "mainnet", &[], |_, _| {
            n += 1;
            Err("down".into())
        })
        .unwrap_err();
        assert_eq!(n, 3);
        assert!(e.contains("no Kaspa resolver named a mainnet node") && e.contains("https://r3: down"), "{e}");
        // an HTTP error is a failure too
        let e = resolve_in(&order[..1], "mainnet", &[], |_, _| Ok((503, json!({"error": "busy"})))).unwrap_err();
        assert!(e.contains("HTTP 503"), "{e}");
    }
}
