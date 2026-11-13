//! Public Kaspa nodes as the default node of the command line.
//!
//! With `--node` unset, the Kaspa resolvers name two public nodes, as for `kobd --node resolver`.
//! The public nodes serve the Borsh wRPC encoding only, so the JSON-shaped calls of `wrpc` are
//! carried over it.
//! - [`bridge`], [`frame`] and [`client`] carry the calls.
//! - [`resolver`] names the nodes.
//! - [`pair`] asks two nodes on different hosts and compares what a transaction is planned from.
//!
//! The code of `bridge`, `frame` and `resolver` follows `crates/kobd/src/rpc/`, whose async
//! clients carry the same calls.

pub mod bridge;
pub mod client;
pub mod frame;
pub mod pair;
pub mod resolver;
