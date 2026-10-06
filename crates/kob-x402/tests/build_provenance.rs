//! Guard against cross-checkout artifact mixing (a shared `CARGO_TARGET_DIR` between worktrees): the `kob-x402` library
//! under test and this test binary must come from the same checkout. See `families_support::assert_single_checkout`.
#![allow(clippy::too_many_arguments)]

#[path = "../../kob-protocol/tests/common/mod.rs"]
mod common;
#[path = "families_support/mod.rs"]
mod support;

#[test]
fn the_library_under_test_and_this_binary_come_from_one_checkout() {
    support::assert_single_checkout(env!("CARGO_MANIFEST_DIR"));
}
