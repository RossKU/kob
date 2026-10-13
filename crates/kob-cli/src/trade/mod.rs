//! The trading commands: `kob place`, `kob replace`, `kob cancel`, `kob refund`, `kob take`, `kob send`.
//!
//! Every command reads the node and the indexer, plans the transaction with the same rules and defaults as the web wallet
//! (`plan`), shows what it does (`show`), and signs only after `--yes` or a terminal's confirmation, with a key read from
//! a file or the environment. Every signed transaction is validated in the script engine before it is submitted.

pub mod actions;
pub mod data;
pub mod flow;
pub mod http;
pub mod market;
pub mod opts;
pub mod place;
pub mod plan;
pub mod plan_pair;
pub mod show;
pub mod units;

#[cfg(test)]
mod tests_e2e;
#[cfg(test)]
mod tests_plan;
#[cfg(test)]
mod tests_plan_kinds;
