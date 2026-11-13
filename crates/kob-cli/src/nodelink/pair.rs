//! A pair of public nodes on two different hosts, as the executor uses them.
//!
//! The executor calls them the authority and a cross-checking node, see `rpc::multi`. The command
//! line asks both for what it signs from and compares the answers:
//! - The network that the nodes report (`getBlockDagInfo`) must agree.
//! - The unspent outputs that it plans a transaction from (`getUtxosByAddresses`) must agree.
//!
//! So one dishonest or lagging public node cannot make the command line sign against outputs
//! that are not there, or against another network.
//!
//! A transaction goes to the first node, and then, as a best effort, to the second. A node that
//! cannot be reached is replaced by another that the resolvers name.
//!
//! The logic runs over [`Io`], which holds the resolvers and the connection, so tests drive it
//! with fakes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use super::client::BorshConn;
use super::resolver;
use crate::wrpc::{self, NodeUtxo, WrpcError, ADDRESS_CHUNK};

/// How many times the two nodes are asked when they disagree, because they may be a block apart.
const AGREE_TRIES: usize = 4;
/// The pause between two such asks.
const AGREE_PAUSE: Duration = Duration::from_millis(1500);
/// How many times a node that cannot be reached is replaced before the call fails.
const REPLACE_TRIES: usize = 3;

/// One call to one node, as URL, method, JSON parameters and timeout.
pub type CallFn<'a> = dyn Fn(&str, &str, Value, Duration) -> Result<Value, WrpcError> + 'a;

/// What the pair logic needs of the world.
pub struct Io<'a> {
    /// The two nodes as URLs, resolved on first use.
    pub nodes: &'a dyn Fn() -> Result<[String; 2], String>,
    /// Gives a node up because it could not be reached, so the next [`Io::nodes`] names another.
    pub replace: &'a dyn Fn(&str),
    /// One call to one node.
    pub call: &'a CallFn<'a>,
    /// Waits before asking again.
    pub pause: &'a dyn Fn(Duration),
}

fn unreachable_(e: &WrpcError) -> bool {
    matches!(e, WrpcError::Socket(_))
}

fn hosts(a: &str, b: &str) -> String {
    format!("{} and {}", resolver::host(a), resolver::host(b))
}

fn fetch_utxos(io: &Io, url: &str, addresses: &[String], timeout: Duration) -> Result<Vec<NodeUtxo>, WrpcError> {
    let mut out = vec![];
    for chunk in addresses.chunks(ADDRESS_CHUNK) {
        let p = (io.call)(url, "getUtxosByAddresses", json!({ "addresses": chunk }), timeout)?;
        out.extend(wrpc::parse_utxo_entries(&p)?);
    }
    out.sort_by(|a, b| (a.txid, a.index).cmp(&(b.txid, b.index)));
    Ok(out)
}

/// The unspent outputs at `addresses`, read from both nodes.
///
/// They are returned when the nodes agree. When they do not, the nodes are asked again a few
/// times, because they may be a block apart. When they never agree, the error names both hosts.
pub fn utxos(io: &Io, addresses: &[String], timeout: Duration) -> Result<Vec<NodeUtxo>, WrpcError> {
    let mut replaced = 0;
    let mut last = String::new();
    let mut tries = 0;
    while tries < AGREE_TRIES {
        let [a, b] = (io.nodes)().map_err(WrpcError::Resolve)?;
        match (fetch_utxos(io, &a, addresses, timeout), fetch_utxos(io, &b, addresses, timeout)) {
            (Ok(x), Ok(y)) if x == y => return Ok(x),
            (Ok(x), Ok(y)) => {
                last = format!(
                    "the public nodes {} disagree about the unspent outputs ({} against {} of the {} address(es)); they may be a block apart: try again",
                    hosts(&a, &b),
                    x.len(),
                    y.len(),
                    addresses.len()
                );
                tries += 1;
                if tries < AGREE_TRIES {
                    (io.pause)(AGREE_PAUSE);
                }
            }
            (Err(e), _) if unreachable_(&e) && replaced < REPLACE_TRIES => {
                (io.replace)(&a);
                replaced += 1;
            }
            (_, Err(e)) if unreachable_(&e) && replaced < REPLACE_TRIES => {
                (io.replace)(&b);
                replaced += 1;
            }
            (Err(e), _) | (_, Err(e)) => return Err(e),
        }
    }
    Err(WrpcError::Protocol(last))
}

/// The network that both nodes report (`getBlockDagInfo`).
///
/// It is an error when they report different ones.
pub fn network(io: &Io, timeout: Duration) -> Result<String, WrpcError> {
    let mut replaced = 0;
    loop {
        let [a, b] = (io.nodes)().map_err(WrpcError::Resolve)?;
        let ask = |url: &str| -> Result<String, WrpcError> {
            let p = (io.call)(url, "getBlockDagInfo", json!({}), timeout)?;
            p.get("network")
                .and_then(Value::as_str)
                .map(wrpc::normalize_network)
                .filter(|n| !n.is_empty())
                .ok_or_else(|| WrpcError::Protocol("getBlockDagInfo names no network".into()))
        };
        match (ask(&a), ask(&b)) {
            (Ok(x), Ok(y)) if x == y => return Ok(x),
            (Ok(x), Ok(y)) => {
                return Err(WrpcError::Protocol(format!("the public nodes {} report different networks ({x}, {y})", hosts(&a, &b))))
            }
            (Err(e), _) if unreachable_(&e) && replaced < REPLACE_TRIES => {
                (io.replace)(&a);
                replaced += 1;
            }
            (_, Err(e)) if unreachable_(&e) && replaced < REPLACE_TRIES => {
                (io.replace)(&b);
                replaced += 1;
            }
            (Err(e), _) | (_, Err(e)) => return Err(e),
        }
    }
}

/// One call to the first node that answers.
///
/// It is for reads that are not signed from, such as the DAA score for a clock.
pub fn call_any(io: &Io, method: &str, params: Value, timeout: Duration) -> Result<Value, WrpcError> {
    let mut replaced = 0;
    loop {
        let [a, b] = (io.nodes)().map_err(WrpcError::Resolve)?;
        match (io.call)(&a, method, params.clone(), timeout) {
            Err(e) if unreachable_(&e) => {
                // The second node answers meanwhile, and the first is given up.
                match (io.call)(&b, method, params.clone(), timeout) {
                    Ok(v) => {
                        (io.replace)(&a);
                        return Ok(v);
                    }
                    Err(e2) if unreachable_(&e2) && replaced < REPLACE_TRIES => {
                        (io.replace)(&a);
                        (io.replace)(&b);
                        replaced += 1;
                    }
                    Err(e2) => return Err(e2),
                }
            }
            other => return other,
        }
    }
}

/// Submits a transaction to the first node, then to the second, and returns the accepted
/// transaction id.
///
/// The answer of the second node is ignored.
pub fn submit(io: &Io, transaction: &Value, allow_orphan: bool, timeout: Duration) -> Result<String, WrpcError> {
    let params = json!({ "transaction": transaction, "allowOrphan": allow_orphan });
    let mut replaced = 0;
    loop {
        let [a, b] = (io.nodes)().map_err(WrpcError::Resolve)?;
        match (io.call)(&a, "submitTransaction", params.clone(), timeout) {
            Ok(p) => {
                let id = wrpc::transaction_id(&p)?;
                let _ = (io.call)(&b, "submitTransaction", params, timeout);
                return Ok(id);
            }
            Err(e) if unreachable_(&e) && replaced < REPLACE_TRIES => {
                (io.replace)(&a);
                replaced += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The public nodes of each network in use in this process, and the hosts that failed.
#[derive(Default)]
struct State {
    pairs: HashMap<String, Vec<String>>,
    failed: Vec<String>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|e| e.into_inner())
}

/// The two nodes of `network`, resolved on first use.
///
/// They are on two different hosts. The second is on the host of the first only when the
/// resolvers name no other. A host that failed in this process is avoided while another is named.
fn real_nodes(network: &str) -> Result<[String; 2], String> {
    let mut s = lock();
    loop {
        let have = s.pairs.get(network).cloned().unwrap_or_default();
        if have.len() >= 2 {
            return Ok([have[0].clone(), have[1].clone()]);
        }
        let mut skip = s.failed.clone();
        skip.extend(have.iter().map(|u| resolver::host(u)));
        let r = resolver::resolve(network, &skip)?;
        if have.contains(&r.url) {
            return Err(format!("the Kaspa resolvers name one public {network} node only ({})", resolver::host(&r.url)));
        }
        s.pairs.entry(network.to_string()).or_default().push(r.url);
    }
}

fn real_replace(network: &str, url: &str) {
    let mut s = lock();
    let h = resolver::host(url);
    if !s.failed.contains(&h) {
        s.failed.push(h);
        if s.failed.len() > 8 {
            s.failed.remove(0);
        }
    }
    if let Some(p) = s.pairs.get_mut(network) {
        p.retain(|u| u != url);
    }
}

fn real_call(url: &str, method: &str, params: Value, timeout: Duration) -> Result<Value, WrpcError> {
    let mut c = BorshConn::connect(url, timeout)?;
    let r = c.call(method, params);
    c.close();
    r
}

/// Runs `f` with the real [`Io`] of `network`.
pub fn with_real<T>(network: &str, f: impl FnOnce(&Io) -> T) -> T {
    let nodes = || real_nodes(network);
    let replace = |u: &str| real_replace(network, u);
    let pause = |d: Duration| std::thread::sleep(d);
    f(&Io { nodes: &nodes, replace: &replace, call: &real_call, pause: &pause })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    fn utxo(tag: u8, amount: u64) -> NodeUtxo {
        NodeUtxo {
            address: None,
            txid: [tag; 32],
            index: 0,
            amount,
            spk: vec![0, 0, 0xac],
            daa: 1,
            coinbase: false,
            covenant_id: None,
        }
    }

    fn entries(us: &[NodeUtxo]) -> Value {
        json!({"entries": us.iter().map(wrpc::utxo_entry_json).collect::<Vec<_>>()})
    }

    /// Two nodes `wss://a/..` and `wss://b/..` answering from scripts; `c` is the replacement the resolvers name.
    struct World {
        answers: RefCell<std::collections::HashMap<String, VecDeque<Result<Value, WrpcError>>>>,
        nodes: RefCell<Vec<String>>,
        replaced: RefCell<Vec<String>>,
        spare: RefCell<VecDeque<String>>,
        asked: RefCell<Vec<(String, String)>>,
        pauses: RefCell<usize>,
    }

    fn url(h: &str) -> String {
        format!("wss://{h}.example/kaspa/mainnet/wrpc/borsh")
    }

    impl World {
        fn new(scripts: Vec<(&str, Vec<Result<Value, WrpcError>>)>, spare: &[&str]) -> World {
            let nodes: Vec<String> = scripts.iter().take(2).map(|(h, _)| url(h)).collect();
            World {
                answers: RefCell::new(scripts.into_iter().map(|(h, a)| (url(h), a.into())).collect()),
                nodes: RefCell::new(nodes),
                replaced: RefCell::new(vec![]),
                spare: RefCell::new(spare.iter().map(|h| url(h)).collect()),
                asked: RefCell::new(vec![]),
                pauses: RefCell::new(0),
            }
        }
        fn run<T>(&self, f: impl FnOnce(&Io) -> T) -> T {
            let nodes = || {
                let mut n = self.nodes.borrow_mut();
                while n.len() < 2 {
                    n.push(self.spare.borrow_mut().pop_front().ok_or("no spare node")?);
                }
                Ok([n[0].clone(), n[1].clone()])
            };
            let replace = |u: &str| {
                self.replaced.borrow_mut().push(u.to_string());
                self.nodes.borrow_mut().retain(|x| x != u);
            };
            let call = |u: &str, m: &str, _p: Value, _t: Duration| {
                self.asked.borrow_mut().push((resolver::host(u), m.to_string()));
                self.answers
                    .borrow_mut()
                    .get_mut(u)
                    .and_then(|q| if q.len() > 1 { q.pop_front() } else { q.front().cloned() })
                    .unwrap_or_else(|| Err(WrpcError::Socket("connection refused".into())))
            };
            let pause = |_d: Duration| *self.pauses.borrow_mut() += 1;
            f(&Io { nodes: &nodes, replace: &replace, call: &call, pause: &pause })
        }
    }

    const T: Duration = Duration::from_secs(1);
    fn addrs() -> Vec<String> {
        vec!["kaspa:qq".to_string()]
    }

    #[test]
    fn agreeing_nodes_give_their_utxos() {
        let us = [utxo(2, 5), utxo(1, 7)];
        let w = World::new(vec![("a", vec![Ok(entries(&us))]), ("b", vec![Ok(entries(&[us[1].clone(), us[0].clone()]))])], &[]);
        let got = w.run(|io| utxos(io, &addrs(), T)).unwrap();
        // The outputs are sorted by outpoint, whatever order the nodes answered in.
        assert_eq!(got.iter().map(|u| u.txid[0]).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(*w.pauses.borrow(), 0);
    }

    #[test]
    fn nodes_a_block_apart_are_asked_again_and_a_standing_disagreement_is_an_error() {
        let (x, y) = ([utxo(1, 7)], [utxo(1, 7), utxo(2, 5)]);
        // The second node catches up on the second round.
        let w = World::new(vec![("a", vec![Ok(entries(&y))]), ("b", vec![Ok(entries(&x)), Ok(entries(&y))])], &[]);
        assert_eq!(w.run(|io| utxos(io, &addrs(), T)).unwrap().len(), 2);
        assert_eq!(*w.pauses.borrow(), 1);
        // It never does, so there is no signing from a view that the nodes do not share.
        let w = World::new(vec![("a", vec![Ok(entries(&y))]), ("b", vec![Ok(entries(&x))])], &[]);
        let e = w.run(|io| utxos(io, &addrs(), T)).unwrap_err();
        assert!(matches!(&e, WrpcError::Protocol(m) if m.contains("a.example and b.example disagree")), "{e}");
        assert_eq!(*w.pauses.borrow(), AGREE_TRIES - 1);
    }

    #[test]
    fn a_node_that_cannot_be_reached_is_replaced() {
        let us = [utxo(1, 7)];
        let w = World::new(
            vec![("a", vec![Err(WrpcError::Socket("refused".into()))]), ("b", vec![Ok(entries(&us))]), ("c", vec![Ok(entries(&us))])],
            &["c"],
        );
        assert_eq!(w.run(|io| utxos(io, &addrs(), T)).unwrap(), us);
        assert_eq!(*w.replaced.borrow(), [url("a")]);
        // With no replacement left, the error of the call is returned.
        let w = World::new(vec![("a", vec![Err(WrpcError::Socket("refused".into()))]), ("b", vec![Ok(entries(&us))])], &[]);
        assert!(w.run(|io| utxos(io, &addrs(), T)).is_err());
        // A refusal by the node is not a reason to replace it.
        let w = World::new(vec![("a", vec![Err(WrpcError::Node("bad".into()))]), ("b", vec![Ok(entries(&us))])], &["c"]);
        assert!(matches!(w.run(|io| utxos(io, &addrs(), T)).unwrap_err(), WrpcError::Node(_)));
        assert!(w.replaced.borrow().is_empty());
    }

    #[test]
    fn both_nodes_must_run_the_network() {
        let dag = |n: &str| json!({"network": n, "virtualDaaScore": 5});
        let w = World::new(vec![("a", vec![Ok(dag("mainnet"))]), ("b", vec![Ok(dag("mainnet"))])], &[]);
        assert_eq!(w.run(|io| network(io, T)).unwrap(), "mainnet");
        let w = World::new(vec![("a", vec![Ok(dag("mainnet"))]), ("b", vec![Ok(dag("testnet-10"))])], &[]);
        let e = w.run(|io| network(io, T)).unwrap_err();
        assert!(matches!(&e, WrpcError::Protocol(m) if m.contains("report different networks (mainnet, testnet-10)")), "{e}");
        // The spellings compare equal.
        let w = World::new(vec![("a", vec![Ok(dag("kaspa-testnet-10"))]), ("b", vec![Ok(dag("testnet-10"))])], &[]);
        assert_eq!(w.run(|io| network(io, T)).unwrap(), "testnet-10");
    }

    #[test]
    fn a_transaction_goes_to_the_first_node_then_the_second() {
        let ok = |id: &str| Ok(json!({"transactionId": id.repeat(64)}));
        let w = World::new(vec![("a", vec![ok("1")]), ("b", vec![Err(WrpcError::Node("already in the mempool".into()))])], &[]);
        let id = w.run(|io| submit(io, &json!({}), false, T)).unwrap();
        assert_eq!(id, "1".repeat(64));
        assert_eq!(w.asked.borrow().iter().map(|(h, _)| h.as_str()).collect::<Vec<_>>(), ["a.example", "b.example"]);
        // The first node is down, so the replacement pair takes it and b stays. The transaction
        // goes out once accepted.
        let w = World::new(
            vec![("a", vec![Err(WrpcError::Socket("refused".into()))]), ("b", vec![ok("2")]), ("c", vec![ok("2")])],
            &["c"],
        );
        assert_eq!(w.run(|io| submit(io, &json!({}), false, T)).unwrap(), "2".repeat(64));
        // A refusal by the node is the answer.
        let w = World::new(vec![("a", vec![Err(WrpcError::Node("orphan".into()))]), ("b", vec![ok("3")])], &[]);
        assert!(matches!(w.run(|io| submit(io, &json!({}), false, T)).unwrap_err(), WrpcError::Node(m) if m == "orphan"));
        assert_eq!(w.asked.borrow().len(), 1, "a refused transaction is not tried on the other node");
    }

    #[test]
    fn a_read_falls_over_to_the_second_node() {
        let dag = json!({"network": "mainnet", "virtualDaaScore": 9});
        let w = World::new(vec![("a", vec![Err(WrpcError::Socket("refused".into()))]), ("b", vec![Ok(dag.clone())])], &[]);
        assert_eq!(w.run(|io| call_any(io, "getBlockDagInfo", json!({}), T)).unwrap(), dag);
        assert_eq!(*w.replaced.borrow(), [url("a")]);
    }
}
