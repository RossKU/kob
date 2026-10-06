//! Integration tests live in `tests/`; this crate has no library code.
//!
//! The tests compile the `.sil` sources under `contracts/` and execute the resulting
//! scripts in rusty-kaspa's `TxScriptEngine` with covenant context and script-unit metering.
