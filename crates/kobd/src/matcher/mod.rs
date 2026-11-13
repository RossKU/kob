//! The KOB matcher, as specified in `docs/spec/matcher.md`.
//!
//! # Modules
//!
//! - [`book`]: the [`book::OrderBookView`] trait the indexer implements, and the in-memory and
//!   snapshot books.
//! - [`family`]: token families, their slot limits per program, and the adapters that lower plans.
//! - [`candidate`]: orders normalised at `t`, with quotes, eligibility, exclusions, priority
//!   classes, trigger evidence and the stops a batch may update.
//! - [`batch`]: the global batch planner. It plans one transaction over every book and pair order,
//!   with netting first, FOK and IOC, auctions, merges, and the stops the batch triggers or arms.
//! - [`auction`]: the auction tick of a fill, the earliest tick at which the batch pays (§3.3).
//! - [`planner`]: plan types and settings, the physical limits, and the fee and update-cost
//!   estimates.
//! - [`lower`]: a plan lowered to a `kob_protocol` batch request, with budgets and exact
//!   accounting.
//! - [`chain`]: chained steps, the book that a transaction leaves for the next one of the tick.
//! - [`pair`]: pair orders (token A for token B), with netting, the two route legs through both
//!   KAS books, and pair trigger evidence.
//! - [`engine`]: one matcher tick. Batches are planned, built, signed and validated in the
//!   rusty-kaspa v2.1.0 engine until nothing crosses.
//! - [`select`]: weighted work selection among competing executors (§3.6).
//! - [`node`]: node access over JSON wRPC, for submission, the DAA score and acceptance.
//! - [`wallet`]: the hot key, from the environment or a permission-checked file, and funding.
//! - [`submit`]: submission, mempool conflicts, acceptance tracking and rollback.
//! - [`journal`]: the in-flight journal, which keeps the tracked transactions on disk across a
//!   restart.
//! - [`run`]: the `kobd match` and `keep` loops, the book sources and the metrics.
//! - [`cli`]: command-line arguments and entry points.

pub mod auction;
pub mod batch;
pub mod book;
pub mod candidate;
pub mod chain;
pub mod cli;
pub mod engine;
pub mod family;
pub mod journal;
pub mod lower;
pub mod node;
pub mod pair;
pub mod planner;
pub mod run;
pub mod select;
pub mod submit;
pub mod wallet;
