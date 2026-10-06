//! C6 "unknown unknowns" harness: [`oracle`] (transaction model, consensus acceptance, value oracle; shared with the
//! planner property tests of `kob-executor`), [`mutate`] (structural mutation operators), [`seeds`] (the seed corpus
//! from the builders' fixtures) and [`fuzz`] (the loop, the corpus, minimisation).

#![allow(dead_code)]

pub mod fuzz;
pub mod intents;
pub mod mutate;
pub mod oracle;
pub mod seeds;

pub use oracle::*;
