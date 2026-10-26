//! `kob-executor` library: the indexer, the matcher, the keepers and the x402 facilitator (one
//! binary, role subcommands; `run` hosts them in one process over one store).

pub mod api;
pub mod app;
pub mod config;
pub mod executor;
pub mod faststart;
pub mod fee;
pub mod hex;
pub mod index_cli;
pub mod indexer;
pub mod keepers;
pub mod logging;
pub mod maintenance;
pub mod matcher;
pub mod model;
pub mod pause;
pub mod recover;
pub mod rpc;
pub mod sanity;
pub mod script;
pub mod setup;
pub mod shutdown;
#[doc(hidden)]
pub mod testkit;
pub mod tokens;
pub mod version;
pub mod wire;
pub mod withdraw;
pub mod x402;
