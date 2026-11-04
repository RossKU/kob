//! The public Kaspa nodes as the default node of the command line (`--node` unset: the Kaspa resolvers name two public nodes,
//! as for `kobd --node resolver`). The public nodes serve the Borsh wRPC encoding only, so the JSON-shaped calls of
//! `wrpc` are carried over it ([`bridge`], [`frame`], [`client`]); [`resolver`] names the nodes and [`pair`] asks two of them on
//! different hosts and compares what a transaction is planned from. The code of `bridge`, `frame` and `resolver` follows
//! `crates/kob-executor/src/rpc/` (the executor's async clients carry the same calls).

pub mod bridge;
pub mod client;
pub mod frame;
pub mod pair;
pub mod resolver;
