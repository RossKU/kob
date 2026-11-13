//! The trading commands and the read-only views.
//!
//! The trading commands are `kob place`, `kob replace`, `kob cancel`, `kob refund`, `kob take`,
//! `kob send` and `kob merge`. The views are `kob book`, `kob orders` and `kob balances`.
//!
//! Every command reads the node and the indexer, plans the transaction with the same rules and
//! defaults as the web wallet (`plan`), and shows what it does (`show`). It signs only after
//! `--yes` or a confirmation at a terminal, with a key read from a file or the environment.
//! Every signed transaction is validated in the script engine before it is submitted.

pub mod actions;
pub mod data;
pub mod flow;
pub mod http;
pub mod market;
pub mod merge;
pub mod opts;
pub mod place;
pub mod plan;
pub mod plan_pair;
pub mod replan;
pub mod show;
pub mod units;
pub mod views;

#[cfg(test)]
mod tests_e2e;
#[cfg(test)]
mod tests_plan;
#[cfg(test)]
mod tests_plan_kinds;
