//! The KOB matcher (`docs/spec/matcher.md`, protocol v2.6).
//!
//! | Module | Contents |
//! |---|---|
//! | [`book`] | the [`book::OrderBookView`] trait the indexer implements, the in-memory / snapshot book |
//! | [`family`] | token families: per-program slot limits, the family adapters that lower plans |
//! | [`candidate`] | orders normalised at `t`: quotes, eligibility, exclusions, priority classes, trigger evidence, updatable stops |
//! | [`batch`] | the global batch planner: one transaction over every book and pair order (netting first, FOK / IOC, auctions, merges, stops triggered by the batch's own fills and the updates arming the others) |
//! | [`planner`] | plan types (fills, updates) and settings, the physical limits, fee and update-cost estimates |
//! | [`lower`] | plan → `kob_protocol` batch request, budgets, exact accounting |
//! | [`chain`] | chained steps: the book after a transaction (continuations for the next transaction of the tick) |
//! | [`pair`] | pair orders (token A for token B): netting participants and the two route legs through both KAS books, pair trigger evidence |
//! | [`engine`] | one matcher tick: batches planned, built, signed and validated in the v2.1.0 engine until nothing crosses |
//! | [`node`] | node access over JSON wRPC (submission, DAA score, VSPC v2 acceptance) |
//! | [`wallet`] | the hot key (environment or permission-checked file, never argv) and funding |
//! | [`submit`] | submission, mempool-conflict handling, acceptance tracking and rollback |
//! | [`run`] | the `kob-executor match` / `keep` loop, book sources, metrics |
//! | [`cli`] | command-line arguments and entry points |

pub mod batch;
pub mod book;
pub mod candidate;
pub mod chain;
pub mod cli;
pub mod engine;
pub mod family;
pub mod lower;
pub mod node;
pub mod pair;
pub mod planner;
pub mod run;
pub mod submit;
pub mod wallet;
